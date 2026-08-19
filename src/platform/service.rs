//! Running an overlay backend on its own thread.
//!
//! The controller window owns the main thread (winit insists), and the overlay
//! must not be built on top of the GUI stack, so the backend lives on a thread
//! of its own and is driven by whole-state snapshots over a channel.

use std::{
    os::fd::{AsFd, OwnedFd},
    sync::{
        mpsc::{self, Receiver, Sender},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use tracing::{debug, error, warn};

use crate::display::OutputInfo;

use super::{
    BackendError, BackendEvent, BackendKind, BackendReport, DesiredState, OverlayBackend,
    Reconciler, Result,
};

enum Command {
    Apply(Box<DesiredState>),
    /// Re-enumerate outputs and rebuild everything.
    Refresh,
    /// Remove every overlay immediately, without touching the profile.
    TearDown,
    Shutdown,
}

/// A handle to the overlay backend running on its own thread.
pub struct OverlayService {
    commands: Sender<Command>,
    events: Receiver<BackendEvent>,
    wake: OwnedFd,
    report: BackendReport,
    outputs: Arc<Mutex<Vec<OutputInfo>>>,
    join: Option<JoinHandle<()>>,
}

impl OverlayService {
    /// Start the backend for `kind`, blocking until it has connected and
    /// enumerated its outputs so the caller can show a populated GUI at once.
    ///
    /// `notify` is called from the backend thread whenever an event is queued,
    /// so a GUI can wake itself up.
    pub fn start(kind: BackendKind, notify: impl Fn() + Send + 'static) -> Result<OverlayService> {
        // Non-blocking, so draining the pipe never stalls the backend thread.
        let (wake_read, wake_write) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::NONBLOCK)
            .map_err(|e| BackendError::Io(e.into()))?;
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let outputs = Arc::new(Mutex::new(Vec::new()));
        let thread_outputs = Arc::clone(&outputs);

        let join = thread::Builder::new()
            .name("unburn-overlay".into())
            .spawn(move || {
                let mut backend = match build(kind) {
                    Ok(backend) => backend,
                    Err(error) => {
                        ready_tx.send(Err(error)).ok();
                        return;
                    }
                };

                let report = backend.report();
                if let Ok(mut slot) = thread_outputs.lock() {
                    *slot = backend.outputs();
                }
                if ready_tx.send(Ok(report)).is_err() {
                    return;
                }

                run(
                    &mut *backend,
                    wake_read,
                    command_rx,
                    event_tx,
                    thread_outputs,
                    notify,
                );
            })
            .map_err(BackendError::Io)?;

        let report = match ready_rx.recv() {
            Ok(Ok(report)) => report,
            Ok(Err(error)) => {
                join.join().ok();
                return Err(error);
            }
            Err(_) => {
                join.join().ok();
                return Err(BackendError::Unavailable(
                    "the overlay backend stopped before it started".into(),
                ));
            }
        };

        Ok(OverlayService {
            commands,
            events,
            wake: wake_write,
            report,
            outputs,
            join: Some(join),
        })
    }

    pub fn report(&self) -> &BackendReport {
        &self.report
    }

    pub fn kind(&self) -> BackendKind {
        self.report.kind
    }

    /// The monitors the backend currently sees.
    pub fn outputs(&self) -> Vec<OutputInfo> {
        self.outputs.lock().map(|o| o.clone()).unwrap_or_default()
    }

    /// Ask for a new on-screen state. Cheap and non-blocking.
    pub fn apply(&self, state: DesiredState) {
        self.send(Command::Apply(Box::new(state)));
    }

    /// Re-enumerate outputs, for instance after a monitor was plugged in.
    pub fn refresh(&self) {
        self.send(Command::Refresh);
    }

    /// Drop every overlay at once, leaving the profile untouched.
    pub fn tear_down(&self) {
        self.send(Command::TearDown);
    }

    /// Events the backend produced since the last call. Never blocks.
    pub fn poll(&self) -> Vec<BackendEvent> {
        self.events.try_iter().collect()
    }

    fn send(&self, command: Command) {
        if self.commands.send(command).is_ok() {
            // Nudge the backend out of its poll.
            rustix::io::write(self.wake.as_fd(), &[1u8]).ok();
        }
    }
}

impl Drop for OverlayService {
    fn drop(&mut self) {
        self.send(Command::Shutdown);
        if let Some(join) = self.join.take() {
            // A crash must remove the overlays, so make sure a clean exit does
            // too before the process goes away.
            if join.join().is_err() {
                error!("the overlay thread panicked; overlays were removed with it");
            }
        }
    }
}

fn build(kind: BackendKind) -> Result<Box<dyn OverlayBackend>> {
    match kind {
        BackendKind::Wayland => Ok(Box::new(super::wayland::WaylandBackend::connect()?)),
        BackendKind::X11 => Ok(Box::new(super::x11::X11Backend::connect()?)),
    }
}

fn run(
    backend: &mut dyn OverlayBackend,
    wake_read: OwnedFd,
    commands: Receiver<Command>,
    events: Sender<BackendEvent>,
    outputs: Arc<Mutex<Vec<OutputInfo>>>,
    notify: impl Fn(),
) {
    let mut reconciler = Reconciler::new();
    let mut pending = Vec::new();

    loop {
        let mut shutdown = false;
        for command in commands.try_iter() {
            match command {
                Command::Apply(state) => reconciler.set_desired(*state),
                Command::Refresh => reconciler.invalidate(),
                Command::TearDown => reconciler.tear_down(backend),
                Command::Shutdown => shutdown = true,
            }
        }
        if shutdown {
            break;
        }

        if let Err(error) = reconciler.sync(backend) {
            error!(%error, "could not update the overlays");
            events
                .send(BackendEvent::Disconnected(error.to_string()))
                .ok();
            notify();
            break;
        }

        pending.clear();
        let animating = reconciler.desired().calibration_disc.is_some();
        if animating {
            // The editor disc keeps spinning without a DesiredState change, so
            // present again even when the reconciler had nothing to do.
            if let Err(error) = backend.flush() {
                error!(%error, "could not present the editor disc");
                events
                    .send(BackendEvent::Disconnected(error.to_string()))
                    .ok();
                notify();
                break;
            }
        }

        // The timeout is only a safety net; everything interesting arrives
        // either on the display server's socket or on the wake pipe. While the
        // on-screen editor is up the disc is rotating, so we wake often enough
        // to keep it moving.
        let timeout = if animating {
            Some(Duration::from_millis(16))
        } else {
            Some(Duration::from_secs(30))
        };
        match backend.poll_events(wake_read.as_fd(), timeout, &mut pending) {
            Ok(()) => {}
            Err(error) => {
                warn!(%error, "the display server connection failed");
                events
                    .send(BackendEvent::Disconnected(error.to_string()))
                    .ok();
                notify();
                break;
            }
        }

        drain(&wake_read);

        let mut woke = false;
        for event in pending.drain(..) {
            if let BackendEvent::OutputsChanged(ref new_outputs) = event {
                if let Ok(mut slot) = outputs.lock() {
                    *slot = new_outputs.clone();
                }
                debug!(count = new_outputs.len(), "the set of outputs changed");
                reconciler.invalidate();
            }
            if events.send(event).is_err() {
                return;
            }
            woke = true;
        }
        if woke {
            notify();
        }
    }

    reconciler.tear_down(backend);
    backend.flush().ok();
}

/// Empty the wake pipe so the next poll actually sleeps.
fn drain(fd: &OwnedFd) {
    let mut scratch = [0u8; 64];
    while let Ok(read) = rustix::io::read(fd.as_fd(), &mut scratch) {
        if read < scratch.len() {
            break;
        }
    }
}

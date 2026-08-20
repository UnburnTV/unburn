//! unburn — display uniformity compensation.

use std::{
    process::ExitCode,
    sync::mpsc::{self, RecvTimeoutError},
    time::Duration,
};

use clap::Parser;
use tracing::info;
use tracing_subscriber::EnvFilter;

use unburn::{
    app::App,
    cli::{Args, Command, OnOff},
    config, gui, ipc, platform,
};

fn main() -> ExitCode {
    let args = Args::parse();
    init_logging(&args);

    match run(args) {
        Ok(code) => code,
        Err(message) => {
            eprintln!("unburn: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<ExitCode, String> {
    match args.command {
        Some(Command::Autostart { state }) => {
            let enable = state == OnOff::On;
            config::set_autostart(enable).map_err(|e| e.to_string())?;
            println!(
                "Start on login: {}",
                if enable { "enabled" } else { "disabled" }
            );
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::Check) => {
            for report in platform::detect() {
                println!("{}\n", report.describe());
            }
            return Ok(ExitCode::SUCCESS);
        }
        Some(Command::Hide | Command::Show | Command::Quit | Command::Status) => {
            return remote_control(&args);
        }
        Some(Command::ListDisplays) => return list_displays(&args),
        Some(Command::Start) | None => {}
    }

    // One instance owns the overlays; a second one just hands over its request.
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let ipc_wake = wake_tx.clone();
    let server = match ipc::Server::bind_notified(move || {
        ipc_wake.send(()).ok();
    }) {
        Ok(server) => server,
        Err(ipc::BindError::AlreadyRunning(_)) => {
            ipc::send(&ipc::Request::ShowWindow)
                .map_err(|e| format!("another instance is running but did not answer: {e}"))?;
            println!("unburn is already running; asked it to show its window.");
            return Ok(ExitCode::SUCCESS);
        }
        Err(error) => return Err(error.to_string()),
    };

    let app = App::start(&args, wake_tx.clone())?;
    if let Some(report) = app.active_report() {
        info!("{}", report.describe().replace('\n', " "));
    }

    if matches!(args.command, Some(Command::Start)) {
        run_headless(app, server, wake_rx)
    } else {
        gui::run(app, server, wake_rx).map(|()| ExitCode::SUCCESS)
    }
}

/// Talk to the instance that owns the overlays, then get out of the way.
fn remote_control(args: &Args) -> Result<ExitCode, String> {
    let request = match args.command {
        Some(Command::Hide) => ipc::Request::Hide,
        Some(Command::Show) => ipc::Request::Show,
        Some(Command::Quit) => ipc::Request::Quit,
        Some(Command::Status) => ipc::Request::Status,
        _ => unreachable!("remote_control is only called for hide/show/quit/status"),
    };

    match ipc::send(&request) {
        Ok(reply) => {
            if !reply.is_empty() {
                println!("{reply}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => Err(format!("no running unburn instance to talk to: {error}")),
    }
}

fn list_displays(args: &Args) -> Result<ExitCode, String> {
    let reports = platform::detect();
    let kind = match args.backend {
        unburn::cli::BackendChoice::Wayland => Some(platform::BackendKind::Wayland),
        unburn::cli::BackendChoice::X11 => Some(platform::BackendKind::X11),
        unburn::cli::BackendChoice::Auto => platform::preferred_kind(&reports),
    };
    let Some(kind) = kind else {
        return Err("no usable overlay backend in this session".into());
    };

    let service = platform::OverlayService::start(kind, || {}).map_err(|e| e.to_string())?;
    println!("{}\n", service.report().describe());

    let outputs = service.outputs();
    if outputs.is_empty() {
        println!("No monitors reported.");
    }
    for output in outputs {
        println!("{}", output.identity.describe());
        println!("  size:      {}×{}", output.width, output.height);
        println!("  position:  {}, {}", output.position.0, output.position.1);
        println!("  scale:     {}", output.scale);
        println!("  transform: {:?}", output.transform);
        if let Some(refresh) = output.refresh_mhz {
            println!("  refresh:   {:.3} Hz", refresh as f64 / 1000.0);
        }
        if let Some(serial) = &output.identity.serial {
            println!("  serial:    {serial}");
        }
        if let Some(hash) = &output.identity.edid_hash {
            println!("  edid:      {hash}");
        }
        println!();
    }
    Ok(ExitCode::SUCCESS)
}

/// Overlays with no window: wait for something to happen, then act on it.
fn run_headless(
    mut app: App,
    server: ipc::Server,
    wake_rx: mpsc::Receiver<()>,
) -> Result<ExitCode, String> {
    info!("running without a calibration window");
    server.publish_status(app.status_line());

    loop {
        for request in server.poll() {
            app.handle_request(&request);
        }
        app.pump();
        server.publish_status(app.status_line());

        if app.should_quit() {
            break;
        }

        // Nothing to do until the backend or the control socket says otherwise.
        match wake_rx.recv_timeout(Duration::from_secs(3600)) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    // Dropping the app tears the overlays down.
    drop(app);
    Ok(ExitCode::SUCCESS)
}

fn init_logging(args: &Args) {
    let filter =
        EnvFilter::try_from_env("UNBURN_LOG").unwrap_or_else(|_| EnvFilter::new(args.log_filter()));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}

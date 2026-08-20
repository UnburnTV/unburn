//! The calibration window.
//!
//! This is an ordinary desktop application window and is deliberately kept
//! separate from the overlay: it only edits the configuration and hands the
//! result to the overlay backend.

pub mod main_window;

use std::{
    sync::mpsc::Receiver,
    thread,
    time::{Duration, Instant},
};

use tracing::warn;

use crate::{app::App, ipc, overlay::TestPattern};

/// How long each grey level stays up in the cycling calibration mode.
const CYCLE_INTERVAL: Duration = Duration::from_millis(1500);

/// Transient interface state that does not belong in the configuration.
#[derive(Default)]
pub struct UiState {
    pub message: Option<(Instant, String)>,
    pub confirm_delete: Option<uuid::Uuid>,
    pub show_advanced: bool,
    pub last_cycle: Option<Instant>,
    /// Show one strength slider per colour channel, even for a neutral spot.
    pub separate_channels: bool,
    /// Which spot's Edit panel is expanded.
    pub params_open: Option<uuid::Uuid>,
    /// Path last copied to the clipboard, so the copy control can stay a check.
    pub path_copied: Option<String>,
}

impl UiState {
    pub fn notice(&mut self, text: impl Into<String>) {
        self.message = Some((Instant::now(), text.into()));
    }

    pub(crate) fn current_message(&self) -> Option<&str> {
        let (at, text) = self.message.as_ref()?;
        (at.elapsed() < Duration::from_secs(6)).then_some(text.as_str())
    }
}

struct UnburnGui {
    app: App,
    server: ipc::Server,
    ui: UiState,
}

/// Open the calibration window and run until it closes.
pub fn run(app: App, server: ipc::Server, wake: Receiver<()>) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("unburn — display compensation")
            .with_inner_size([980.0, 660.0])
            .with_min_inner_size([720.0, 520.0]),
        ..Default::default()
    };

    eframe::run_native(
        "unburn",
        options,
        Box::new(move |cc| {
            cc.egui_ctx
                .all_styles_mut(|style| main_window::apply_ui_scale(style));

            // Anything the backend or the control socket reports arrives on
            // this channel; forward it as a repaint so the window stays asleep
            // in between.
            let ctx = cc.egui_ctx.clone();
            thread::Builder::new()
                .name("unburn-wake".into())
                .spawn(move || {
                    while wake.recv().is_ok() {
                        ctx.request_repaint();
                    }
                })
                .ok();

            Ok(Box::new(UnburnGui {
                ui: UiState::default(),
                app,
                server,
            }))
        }),
    )
    .map_err(|error| format!("could not open the calibration window: {error}"))
}

impl eframe::App for UnburnGui {
    /// The overlay is a separate surface, so the window itself can be opaque.
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    /// Everything that is not drawing: the control socket, the backend's
    /// events, the keyboard shortcuts and the grey-ramp timer.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        for request in self.server.poll() {
            if request == ipc::Request::ShowWindow {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            self.app.handle_request(&request);
        }
        self.app.pump();
        self.server.publish_status(self.app.status_line());

        self.handle_keys(ctx);
        self.advance_pattern_cycle(ctx);

        if self.app.should_quit() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        if self.ui.current_message().is_some() {
            ui.ctx().request_repaint_after(Duration::from_secs(1));
        }

        egui::CentralPanel::default_margins().show(ui, |ui| {
            main_window::draw(ui, &mut self.app, &mut self.ui);
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.app.unsaved_changes() {
            if let Err(error) = self.app.save() {
                warn!(%error, "could not save the configuration on exit");
            }
        }
    }
}

impl UnburnGui {
    /// The calibration keys work from the window too, not only from the
    /// overlay, so a user can keep their hands in one place.
    fn handle_keys(&mut self, ctx: &egui::Context) {
        if self.ui.confirm_delete.is_some() {
            return;
        }
        let (space, left, right, escape) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::Space),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::Escape),
            )
        });

        if space {
            self.app.toggle_bypass();
        }
        if escape {
            if self.app.test_pattern().is_some() {
                self.app.set_test_pattern(None);
            } else if self.app.is_editing() {
                self.app.set_editing(false);
            }
        }
        if left || right {
            if let Some(mut state) = self.app.test_pattern() {
                state.pattern = state.pattern.step(if right { 1 } else { -1 });
                state.cycling = false;
                self.app.set_test_pattern(Some(state));
            }
        }
    }

    fn advance_pattern_cycle(&mut self, ctx: &egui::Context) {
        let Some(mut state) = self.app.test_pattern() else {
            self.ui.last_cycle = None;
            return;
        };
        if !state.cycling {
            self.ui.last_cycle = None;
            return;
        }

        let last = *self.ui.last_cycle.get_or_insert_with(Instant::now);
        if last.elapsed() >= CYCLE_INTERVAL {
            let index = TestPattern::GREYS
                .iter()
                .position(|p| *p == state.pattern)
                .map(|i| (i + 1) % TestPattern::GREYS.len())
                .unwrap_or(0);
            state.pattern = TestPattern::GREYS[index];
            self.app.set_test_pattern(Some(state));
            self.ui.last_cycle = Some(Instant::now());
        }
        ctx.request_repaint_after(CYCLE_INTERVAL / 4);
    }
}

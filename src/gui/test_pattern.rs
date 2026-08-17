//! The calibration pattern controls.

use crate::{
    app::App,
    overlay::{TestPattern, TestPatternState},
};

/// The `[Test pattern ▼]` control and its companions.
pub fn show(ui: &mut egui::Ui, app: &mut App) {
    let current = app.test_pattern();
    let label = match current {
        None => "Test pattern".to_string(),
        Some(state) => state.pattern.label(),
    };

    ui.horizontal(|ui| {
        egui::ComboBox::from_id_salt("test-pattern")
            .selected_text(label)
            .show_ui(ui, |ui| {
                let mut chosen: Option<Option<TestPattern>> = None;
                if ui.selectable_label(current.is_none(), "Off").clicked() {
                    chosen = Some(None);
                }
                ui.separator();
                for pattern in TestPattern::ALL {
                    let selected = current.map(|s| s.pattern) == Some(pattern);
                    if ui.selectable_label(selected, pattern.label()).clicked() {
                        chosen = Some(Some(pattern));
                    }
                }

                if let Some(chosen) = chosen {
                    app.set_test_pattern(chosen.map(|pattern| TestPatternState {
                        pattern,
                        compensated: !app.is_bypassed(),
                        cycling: false,
                    }));
                }
            });

        if let Some(mut state) = current {
            if ui.checkbox(&mut state.cycling, "Cycle grayscale").changed() {
                app.set_test_pattern(Some(state));
            }
        }
    });

    if current.is_some() {
        ui.label(
            egui::RichText::new(
                "Space compares before and after · ← → change pattern · Esc closes",
            )
            .small()
            .weak(),
        );
    }
}

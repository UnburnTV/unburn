//! The calibration window's layout.

use egui::{RichText, Slider};
use uuid::Uuid;

use crate::{
    app::App,
    compensation::{MaskQuality, Rgb},
    config,
    display::DisplayIdentity,
    overlay::ShowMode,
};

use super::{test_pattern, UiState};

/// Space left to the right of a slider's rail for its value box and label.
const SLIDER_LABEL_ROOM: f32 = 230.0;

/// Smallest strength the slider resolves, in percent.
const STRENGTH_FLOOR: f64 = 0.5;

/// A strength slider, in percent.
///
/// The scale is logarithmic because the range it has to cover is: most panels
/// want a few percent, while a badly burnt patch can want several hundred. A
/// linear rail would bury every ordinary setting in its first few pixels.
fn strength_slider(value: &mut f32, limit: f32) -> Slider<'_> {
    Slider::new(value, 0.0..=limit)
        .logarithmic(true)
        .smallest_positive(STRENGTH_FLOOR)
        .suffix(" %")
        .fixed_decimals(1)
}

pub fn draw(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        // Calibration is done by nudging sliders a fraction at a time, so give
        // them the whole window width rather than egui's stubby default.
        let room = ui.available_width() - SLIDER_LABEL_ROOM;
        ui.spacing_mut().slider_width = room.clamp(200.0, 1400.0);

        display_picker(ui, app);
        ui.separator();
        global_sliders(ui, app);
        ui.separator();
        defect_list(ui, app, state);
        ui.separator();
        selected_defect(ui, app, state);
        ui.separator();
        summary(ui, app);
        ui.separator();
        bottom_row(ui, app, state);
    });
}

fn display_picker(ui: &mut egui::Ui, app: &mut App) {
    let outputs = app.outputs();
    let current = app
        .selected_display()
        .map(|d| (d.identity.clone(), d.label()));
    let current_label = current
        .as_ref()
        .map(|(_, label)| label.clone())
        .unwrap_or_else(|| "None".into());

    ui.horizontal(|ui| {
        ui.label("Display:");
        egui::ComboBox::from_id_salt("display")
            .selected_text(current_label)
            .show_ui(ui, |ui| {
                let mut chosen: Option<DisplayIdentity> = None;
                for display in &app.profile.displays {
                    let connected =
                        crate::display::best_match(&display.identity, outputs.iter()).is_some();
                    let label = if connected {
                        display.label()
                    } else {
                        format!("{} (disconnected)", display.label())
                    };
                    let selected = current.as_ref().map(|(id, _)| id) == Some(&display.identity);
                    if ui.selectable_label(selected, label).clicked() {
                        chosen = Some(display.identity.clone());
                    }
                }
                if let Some(chosen) = chosen {
                    app.select_display(chosen);
                }
            });

        if let Some(output) = app.selected_output() {
            ui.label(RichText::new(format!("{}×{}", output.width, output.height)).weak());
        } else {
            ui.label(RichText::new("not connected").weak());
        }
    });

    let Some(display) = app.selected_display() else {
        return;
    };
    let mut enabled = display.enabled;
    let mut name = display.name.clone();

    ui.horizontal(|ui| {
        if ui
            .checkbox(&mut enabled, "Compensate this display")
            .changed()
        {
            if let Some(display) = app.selected_display_mut() {
                display.enabled = enabled;
            }
            app.mark_changed();
        }
        ui.add_space(8.0);
        ui.label("Name:");
        if ui.text_edit_singleline(&mut name).changed() {
            if let Some(display) = app.selected_display_mut() {
                display.name = name;
            }
            app.mark_changed();
        }
    });
}

fn global_sliders(ui: &mut egui::Ui, app: &mut App) {
    let Some(display) = app.selected_display() else {
        return;
    };
    let mut compensation = display.compensation * 100.0;

    let response = ui.add(
        Slider::new(&mut compensation, 0.0..=100.0)
            .text("Compensation")
            .suffix(" %")
            .fixed_decimals(0),
    );
    if response.changed() {
        if let Some(display) = app.selected_display_mut() {
            display.compensation = compensation / 100.0;
        }
        app.mark_changed();
    }
    ui.label(
        RichText::new(
            "At 100% the panel is brought all the way down to its dimmest modelled point.",
        )
        .small()
        .weak(),
    );
}

fn defect_list(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    ui.heading("Defects");

    let Some(display) = app.selected_display() else {
        ui.label("Select a display first.");
        return;
    };
    let entries: Vec<(Uuid, String, bool)> = display
        .defects
        .iter()
        .enumerate()
        .map(|(index, d)| {
            (
                d.id(),
                config::DisplayProfile::defect_label(index),
                d.enabled(),
            )
        })
        .collect();
    let selected = app.selected_defect();

    if entries.is_empty() {
        ui.label(RichText::new("None yet. Add a spot over each blemish.").weak());
    }

    egui::ScrollArea::vertical()
        .max_height(160.0)
        .id_salt("defects")
        .show(ui, |ui| {
            for (id, name, enabled) in &entries {
                ui.horizontal(|ui| {
                    let mut on = *enabled;
                    if ui.checkbox(&mut on, "").changed() {
                        if let Some(display) = app.selected_display_mut() {
                            if let Some(index) = display.defect_index(*id) {
                                display.defects[index].set_enabled(on);
                            }
                        }
                        app.mark_changed();
                    }
                    if ui.selectable_label(selected == Some(*id), name).clicked() {
                        app.select_defect(Some(*id));
                    }
                });
            }
        });

    ui.add_space(4.0);
    ui.horizontal(|ui| {
        if ui.button("+ Add spot").clicked() {
            app.add_defect(crate::compensation::Vec2::splat(0.5));
            state.notice(
                "Added a spot in the centre. Press Edit on screen to drag it onto the blemish.",
            );
        }

        if ui
            .add_enabled(selected.is_some(), egui::Button::new("Clone spot"))
            .on_hover_text("Copy the selected spot, offset so both stay reachable")
            .clicked()
        {
            if let Some(id) = selected {
                app.clone_defect(id);
                state.notice("Cloned the spot beside the original, and selected the copy.");
            }
        }

        let editing = app.is_editing();
        let label = if editing {
            "Stop editing on screen"
        } else {
            "Edit on screen"
        };
        if ui.button(label).clicked() {
            app.set_editing(!editing);
            if !editing {
                state.notice(
                    "The overlay is now interactive: drag a spot to move it, wheel to resize, \
Shift+wheel for strength, n for a new spot, Esc or a click on empty screen to leave.",
                );
            }
        }

        let can_delete = selected.is_some();
        if ui
            .add_enabled(can_delete, egui::Button::new("Delete"))
            .clicked()
        {
            if let Some(id) = selected {
                app.delete_defect(id);
            }
        }
    });

    if app.is_editing() {
        ui.horizontal(|ui| {
            ui.label("On screen:");
            for mode in ShowMode::ALL {
                if ui
                    .selectable_label(app.show_mode() == mode, mode.label())
                    .clicked()
                {
                    app.set_show_mode(mode);
                }
            }
        });
    }
}

fn selected_defect(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    ui.heading("Selected spot");

    let Some(id) = app.selected_defect() else {
        ui.label(RichText::new("Nothing selected.").weak());
        return;
    };
    let Some(radial) = app
        .selected_display()
        .and_then(|d| d.defects.iter().find(|x| x.id() == id))
        .and_then(|d| d.as_radial())
        .cloned()
    else {
        ui.label(RichText::new("Nothing selected.").weak());
        return;
    };

    if !app.is_editing() {
        ui.label(
            RichText::new("Position and size are set on the screen itself: press Edit on screen.")
                .small()
                .weak(),
        );
    }

    strength_sliders(ui, app, state, id, radial.strength);

    let mut falloff = radial.falloff;
    let mut rotation = radial.rotation.to_degrees();
    let mut changed = false;

    changed |= ui
        .add(
            Slider::new(&mut falloff, 0.2..=4.0)
                .text("Falloff")
                .fixed_decimals(2),
        )
        .changed();
    changed |= ui
        .add(
            Slider::new(&mut rotation, -90.0..=90.0)
                .text("Rotation")
                .suffix("°"),
        )
        .changed();

    if changed {
        app.edit_defect(id, |d| {
            d.falloff = falloff;
            d.rotation = rotation.to_radians();
        });
    }
}

/// How much brighter than the rest of the panel the spot is, per channel.
///
/// One slider while the spot is neutral, three once it is not: a tinted patch
/// needs different amounts taken out of each channel, and collapsing that back
/// to a single number would silently throw the calibration away.
fn strength_sliders(
    ui: &mut egui::Ui,
    app: &mut App,
    state: &mut UiState,
    id: Uuid,
    strength: Rgb,
) {
    let mut separate = state.separate_channels || !strength.is_neutral();
    if ui
        .checkbox(&mut separate, "Separate colour channels")
        .on_hover_text("Correct a tinted spot by taking a different amount out of each channel")
        .changed()
    {
        state.separate_channels = separate;
        if !separate {
            // Collapsing keeps the strongest channel, which is the one that
            // was setting the overall brightness of the spot.
            let flattened = Rgb::splat(strength.max_channel());
            app.edit_defect(id, |d| d.strength = flattened);
        }
    }

    let percent = strength * 100.0;
    let mut edited = percent;
    let mut changed = false;
    let limit = crate::app::MAX_STRENGTH * 100.0;

    if separate {
        for (value, label) in [
            (&mut edited.r, "Red"),
            (&mut edited.g, "Green"),
            (&mut edited.b, "Blue"),
        ] {
            changed |= ui.add(strength_slider(value, limit).text(label)).changed();
        }
    } else {
        let mut all = percent.r;
        if ui
            .add(strength_slider(&mut all, limit).text("Strength"))
            .changed()
        {
            edited = Rgb::splat(all);
            changed = true;
        }
    }

    if changed {
        app.edit_defect(id, |d| d.strength = edited * 0.01);
    }
    ui.label(
        RichText::new("How much brighter than the rest of the panel this spot is.")
            .small()
            .weak(),
    );
}

fn bottom_row(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    test_pattern::show(ui, app);
    ui.add_space(6.0);

    ui.horizontal(|ui| {
        let bypassed = app.is_bypassed();
        let label = if bypassed {
            "Compensation OFF"
        } else {
            "Compensation ON"
        };
        let colour = if bypassed {
            egui::Color32::from_rgb(200, 90, 70)
        } else {
            egui::Color32::from_rgb(70, 170, 110)
        };
        if ui
            .add(egui::Button::new(RichText::new(label).color(egui::Color32::WHITE)).fill(colour))
            .on_hover_text("Instantly removes the overlay without recomputing anything (Space)")
            .clicked()
        {
            app.toggle_bypass();
        }

        if ui.button("Save profile").clicked() {
            match app.save() {
                Ok(()) => state.notice(format!("Saved to {}", app.profile_path().display())),
                Err(error) => state.notice(format!("Could not save: {error}")),
            }
        }
        if app.unsaved_changes() {
            ui.label(RichText::new("unsaved changes").weak().small());
        }
    });

    ui.add_space(6.0);
    let mut autostart = config::autostart_enabled();
    if ui
        .checkbox(&mut autostart, "Start automatically on login")
        .changed()
    {
        match config::set_autostart(autostart, app.profile_name()) {
            Ok(()) => state.notice("Updated the login entry."),
            Err(error) => state.notice(format!("Could not update autostart: {error}")),
        }
    }

    ui.collapsing("Advanced", |ui| {
        let Some(display) = app.selected_display() else {
            return;
        };
        let mut quality = display.quality;
        let mut dither = display.dither;
        let mut changed = false;

        ui.horizontal(|ui| {
            ui.label("Mask quality:");
            for option in MaskQuality::ALL {
                changed |= ui
                    .selectable_value(&mut quality, option, option.label())
                    .changed();
            }
        });
        changed |= ui
            .checkbox(&mut dither, "Dither the 8-bit alpha")
            .on_hover_text("A fixed, zero-mean pattern that hides banding without shimmering")
            .changed();

        if changed {
            if let Some(display) = app.selected_display_mut() {
                display.quality = quality;
                display.dither = dither;
            }
            app.mark_changed();
        }

        ui.add_space(6.0);
        for report in app.backend_reports() {
            ui.label(RichText::new(report.describe()).small().weak());
        }
        ui.label(
            RichText::new(format!("Profile: {}", app.profile_path().display()))
                .small()
                .weak(),
        );
    });
}

/// What the current settings cost in brightness, and what the panel looks like.
fn summary(ui: &mut egui::Ui, app: &App) {
    let Some(display) = app.selected_display() else {
        return;
    };
    let params = display.mask_params();
    let mask = crate::compensation::mask::generate_at(&display.defects, &params, 160, 90);

    let spread = mask
        .max_gain
        .zip(mask.min_gain, |hi, lo| hi - lo)
        .max_channel()
        * 100.0;
    let loss = mask.peak_alpha() * 100.0;

    ui.label(format!("Modelled unevenness:   {spread:.1}%"));
    ui.label(format!("Largest light removed: {loss:.1}%"));

    let lift = mask.black_lift() * 100.0;
    if lift > 0.05 {
        ui.label(format!("Black lifted by:       {lift:.1}%"))
            .on_hover_text(
                "The price of per-channel correction: the overlay can only dim every channel by \
the same factor, so the channels that needed less attenuation get their light handed back as a \
faint constant glow. This figure is the worst case, reached only on a fully black screen.",
            );
    }
    if let Some(report) = app.active_report() {
        ui.label(RichText::new(report.describe()).small().weak());
    }
    if app.is_bypassed() {
        ui.label(RichText::new("Compensation is currently bypassed.").small());
    }
}

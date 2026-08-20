//! The calibration window's layout.

use egui::{pos2, RichText, Slider, StrokeKind, Vec2};
use uuid::Uuid;

use crate::{
    app::App,
    compensation::{MaskQuality, RadialDefect, Rgb},
    config,
    display::DisplayIdentity,
    overlay::{DiscSwatch, ShowMode},
};

use super::UiState;

/// Space left to the right of a slider's rail for its value box and label.
const SLIDER_LABEL_ROOM: f32 = 230.0;

/// Smallest strength the slider resolves, in percent.
const STRENGTH_FLOOR: f64 = 0.5;

/// Slot reserved for a stroked glyph on the spot-list buttons.
const ICON_ATOM: &str = "spot_btn_icon";
const ICON_SIZE: f32 = 18.5;

/// How much larger the calibration window is than egui's defaults.
const UI_SCALE: f32 = 1.4;

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

/// The slider rail uses `inactive.bg_fill`. Hover/active handle fills stay
/// on the theme so the grab is not painted twice.
fn add_strength_slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    limit: f32,
    rail: Option<egui::Color32>,
) -> egui::Response {
    let Some(rail) = rail else {
        return ui.add(strength_slider(value, limit));
    };
    ui.scope(|ui| {
        ui.visuals_mut().widgets.inactive.bg_fill = rail;
        ui.add(strength_slider(value, limit))
    })
    .inner
}

pub fn draw(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    status_bar(ui, app, state);
    egui::ScrollArea::vertical().show(ui, |ui| {
        // Calibration is done by nudging sliders a fraction at a time, so give
        // them the whole window width rather than egui's stubby default.
        let room = ui.available_width() - SLIDER_LABEL_ROOM;
        ui.spacing_mut().slider_width = room.clamp(200.0, 1400.0);

        display_picker(ui, app);
        ui.separator();
        defect_list_inner(ui, app, state);
        ui.separator();
        bottom_row(ui, app, state);
    });
    confirm_delete_dialog(ui, app, state);
}

pub fn apply_ui_scale(style: &mut egui::Style) {
    style.spacing.interact_size *= UI_SCALE;
    style.spacing.button_padding *= UI_SCALE;
    style.spacing.icon_width *= UI_SCALE;
    style.spacing.icon_width_inner *= UI_SCALE;
    style.spacing.icon_spacing *= UI_SCALE;
    style.spacing.item_spacing *= UI_SCALE;
    for font in style.text_styles.values_mut() {
        font.size *= UI_SCALE;
    }
}

fn status_bar(ui: &mut egui::Ui, app: &App, state: &UiState) {
    egui::Panel::bottom("status").show(ui, |ui| {
        ui.horizontal(|ui| {
            if let Some(message) = state.current_message() {
                ui.label(message);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let (line, detail) = backend_status(app);
                let response = ui.label(RichText::new(line).small().weak());
                if !detail.is_empty() {
                    response.on_hover_text(detail);
                }
            });
        });
    });
}

fn backend_status(app: &App) -> (String, String) {
    if let Some(report) = app.active_report() {
        return backend_status_line(report);
    }
    let lines: Vec<(String, String)> = app
        .backend_reports()
        .iter()
        .map(backend_status_line)
        .collect();
    let line = lines
        .iter()
        .map(|(l, _)| l.as_str())
        .collect::<Vec<_>>()
        .join("  |  ");
    let detail = lines
        .iter()
        .filter(|(_, d)| !d.is_empty())
        .map(|(_, d)| d.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (line, detail)
}

fn backend_status_line(report: &crate::platform::BackendReport) -> (String, String) {
    let line = format!(
        "{} support: {}",
        report.kind.label(),
        report.support.headline()
    );
    let detail = report.support.detail().unwrap_or("").to_string();
    if detail.is_empty() {
        (line, String::new())
    } else {
        (format!("{line} - {detail}"), detail)
    }
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
                for display in &app.config.displays {
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

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            enable_toggle(ui, app);
        });
    });
}

fn enable_toggle(ui: &mut egui::Ui, app: &mut App) {
    ui.horizontal(|ui| {
        ui.label("Enable");
        let mut on = !app.is_bypassed();
        if toggle_switch(ui, &mut on)
            .on_hover_text("Instantly removes the overlay without recomputing anything (Space)")
            .changed()
        {
            app.set_bypass(!on);
        }
    });
}

fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let height = ui.spacing().interact_size.y;
    let (rect, mut response) =
        ui.allocate_exact_size(Vec2::new(height * 1.85, height), egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let off = egui::Color32::from_rgb(110, 110, 118);
        let on_fill = egui::Color32::from_rgb(70, 170, 110);
        let fill = egui::Color32::from_rgb(
            mix_u8(off.r(), on_fill.r(), how_on),
            mix_u8(off.g(), on_fill.g(), how_on),
            mix_u8(off.b(), on_fill.b(), how_on),
        );
        let radius = 0.5 * rect.height();
        ui.painter()
            .rect(rect, radius, fill, egui::Stroke::NONE, StrokeKind::Inside);
        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        ui.painter().circle_filled(
            pos2(circle_x, rect.center().y),
            0.72 * radius,
            egui::Color32::WHITE,
        );
    }
    response
}

fn mix_u8(a: u8, b: u8, t: f32) -> u8 {
    (a as f32 + (b as f32 - a as f32) * t).round() as u8
}

fn defect_list_inner(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    ui.heading("Compensated Defects");

    let Some(display) = app.selected_display() else {
        ui.label("Select a display first.");
        app.set_hovered_defect(None);
        return;
    };
    let entries: Vec<(Uuid, String, bool)> = display
        .defects
        .iter()
        .enumerate()
        .map(|(index, d)| {
            (
                d.id(),
                config::DisplayConfig::defect_label(index),
                d.enabled(),
            )
        })
        .collect();

    if entries.is_empty() {
        ui.label(RichText::new("None yet. Add a spot over each blemish.").weak());
    }

    let editing = app.is_editing();
    let active = app.selected_defect();
    let delete_fill = egui::Color32::from_rgb(200, 90, 70);
    let params_open = state.params_open;
    let mut hovered = None;

    for (id, name, enabled) in &entries {
        let params_this = params_open == Some(*id);
        let moving_this = editing && active == Some(*id);
        let locate = !params_this && !moving_this;

        let hit = ui
            .horizontal(|ui| {
                let mut on = *enabled;
                let checkbox = ui.checkbox(&mut on, "");
                if checkbox.changed() {
                    if let Some(display) = app.selected_display_mut() {
                        if let Some(index) = display.defect_index(*id) {
                            display.defects[index].set_enabled(on);
                        }
                    }
                    app.mark_changed();
                }
                let label = ui.label(name);

                let edit = icon_button("Edit").selected(params_this).atom_ui(ui);
                paint_icon(ui, &edit, BtnIcon::Edit, None);
                let edit_resp = edit.response.on_hover_text(
                    "Show strength, falloff and preview disc colours for this spot",
                );
                if edit_resp.clicked() {
                    if params_this {
                        state.params_open = None;
                        app.set_calibration_disc(None);
                    } else {
                        if app.is_editing() {
                            app.set_editing(false);
                        }
                        state.params_open = Some(*id);
                        app.set_calibration_disc(Some(*id));
                    }
                }

                let move_btn = icon_button("Move").selected(moving_this).atom_ui(ui);
                paint_icon(ui, &move_btn, BtnIcon::Move, None);
                let move_resp = move_btn.response.on_hover_text(
                    "Drag this spot on the screen: wheel to resize, Shift+wheel for \
strength, Esc or a click on empty screen to leave",
                );
                if move_resp.clicked() {
                    if moving_this {
                        app.set_editing(false);
                    } else {
                        if state.params_open.is_some() {
                            state.params_open = None;
                            app.set_calibration_disc(None);
                        }
                        app.select_defect(Some(*id));
                        app.set_editing(true);
                        state.notice(
                            "The overlay is now interactive: drag the spot onto the blemish, \
wheel to resize, Shift+wheel for strength, n for a new spot, Esc or a click on empty screen to leave. \
The correction is not drawn while moving; it comes back when you leave.",
                        );
                    }
                }

                let clone = icon_button("Clone").atom_ui(ui);
                paint_icon(ui, &clone, BtnIcon::Clone, None);
                let clone_resp = clone
                    .response
                    .on_hover_text("Copy this spot, offset so both stay reachable");
                if clone_resp.clicked() {
                    app.clone_defect(*id);
                    state.notice("Cloned the spot beside the original.");
                }

                let delete = icon_button(RichText::new("Delete").color(egui::Color32::WHITE))
                    .fill(delete_fill)
                    .atom_ui(ui);
                paint_icon(ui, &delete, BtnIcon::Delete, Some(egui::Color32::WHITE));
                if delete.response.clicked() {
                    state.confirm_delete = Some(*id);
                }

                checkbox
                    .union(label)
                    .union(edit_resp)
                    .union(move_resp)
                    .union(clone_resp)
                    .union(delete.response)
            })
            .inner;

        if locate && hit.hovered() {
            hovered = Some(*id);
        }

        let moving_this = app.is_editing() && app.selected_defect() == Some(*id);
        let params_this = state.params_open == Some(*id);
        if moving_this || params_this {
            if let Some(radial) = app
                .selected_display()
                .and_then(|d| d.defects.iter().find(|d| d.id() == *id))
                .and_then(|d| d.as_radial())
                .cloned()
            {
                ui.indent(*id, |ui| {
                    if moving_this {
                        ui.group(|ui| {
                            rotation_slider(ui, app, *id, radial.rotation);
                        });
                    }
                    if params_this {
                        ui.group(|ui| {
                            spot_params(ui, app, state, *id, radial);
                        });
                    }
                });
            }
        }
    }

    app.set_hovered_defect(hovered);

    ui.add_space(4.0);
    let add = icon_button("Add spot").atom_ui(ui);
    paint_icon(ui, &add, BtnIcon::Add, None);
    if add.response.clicked() {
        app.add_defect(crate::compensation::Vec2::splat(0.5));
        state.notice("Added a spot in the centre. Press Move to drag it onto the blemish.");
    }

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

fn spot_params(
    ui: &mut egui::Ui,
    app: &mut App,
    state: &mut UiState,
    id: Uuid,
    radial: RadialDefect,
) {
    size_sliders_for_table(ui);
    strength_and_falloff(ui, app, state, id, radial);
    ui.add_space(8.0);
    disc_color_pickers(ui, app);
}

fn rotation_slider(ui: &mut egui::Ui, app: &mut App, id: Uuid, rotation: f32) {
    size_sliders_for_table(ui);
    let mut degrees = rotation.to_degrees();
    egui::Grid::new(("spot_rotation", id))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Rotation");
            if ui
                .add(Slider::new(&mut degrees, -90.0..=90.0).suffix("°"))
                .changed()
            {
                app.edit_defect(id, |d| d.rotation = degrees.to_radians());
            }
            ui.end_row();
        });
}

/// Leave the value box in the slider cell so every rail starts at the same x.
fn size_sliders_for_table(ui: &mut egui::Ui) {
    let room = ui.available_width() - 160.0;
    ui.spacing_mut().slider_width = room.clamp(120.0, 1400.0);
}

/// Colours on the rotating disc behind the spot. Several ticked boxes split
/// the disc into equal wedges; none leaves it empty.
fn disc_color_pickers(ui: &mut egui::Ui, app: &mut App) {
    ui.label("Preview Disc Colors");
    let mut flags = [false; DiscSwatch::ALL.len()];
    for (i, swatch) in DiscSwatch::ALL.iter().enumerate() {
        flags[i] = app.disc_colors().iter().any(|c| *c == swatch.rgb);
    }

    let mut changed = false;
    egui::Grid::new("preview_disc_colors")
        .num_columns(6)
        .spacing([12.0, 6.0])
        .min_col_width(0.0)
        .show(ui, |ui| {
            for i in 0..6 {
                changed |= colored_checkbox(ui, &mut flags[i], DiscSwatch::ALL[i]).changed();
            }
            ui.end_row();
            for i in 6..DiscSwatch::ALL.len() {
                changed |= colored_checkbox(ui, &mut flags[i], DiscSwatch::ALL[i]).changed();
            }
            ui.end_row();
        });
    if changed {
        app.set_disc_colors(DiscSwatch::selected(&flags));
    }
}

/// A checkbox whose box is filled with the swatch colour. The label uses the
/// theme text colour so every name reads the same.
fn colored_checkbox(ui: &mut egui::Ui, on: &mut bool, swatch: DiscSwatch) -> egui::Response {
    let fill = egui::Color32::from_rgb(swatch.rgb[0], swatch.rgb[1], swatch.rgb[2]);
    let luma =
        u16::from(swatch.rgb[0]) * 3 + u16::from(swatch.rgb[1]) * 6 + u16::from(swatch.rgb[2]);
    let check = if luma < 1280 {
        egui::Color32::WHITE
    } else {
        egui::Color32::BLACK
    };
    ui.horizontal(|ui| {
        let box_response = ui
            .scope(|ui| {
                let border = egui::Stroke::new(1.5, egui::Color32::from_gray(140));
                let mark = egui::Stroke::new(2.0, check);
                let widgets = &mut ui.visuals_mut().widgets;
                for visuals in [
                    &mut widgets.noninteractive,
                    &mut widgets.inactive,
                    &mut widgets.hovered,
                    &mut widgets.active,
                ] {
                    visuals.bg_fill = fill;
                    visuals.weak_bg_fill = fill;
                    visuals.bg_stroke = border;
                    visuals.fg_stroke = mark;
                }
                ui.add(egui::Checkbox::without_text(on))
            })
            .inner;
        let label = ui.label(swatch.label);
        let label_clicked = label.clicked();
        if label_clicked {
            *on = !*on;
        }
        let mut response = box_response.union(label);
        if label_clicked {
            response.mark_changed();
        }
        response
    })
    .inner
}

/// How much brighter than the rest of the panel the spot is, per channel.
///
/// One slider while the spot is neutral, three once it is not: a tinted patch
/// needs different amounts taken out of each channel, and collapsing that back
/// to a single number would silently throw the calibration away.
fn strength_and_falloff(
    ui: &mut egui::Ui,
    app: &mut App,
    state: &mut UiState,
    id: Uuid,
    radial: RadialDefect,
) {
    let strength = radial.strength;
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
    let mut strength_changed = false;
    let limit = crate::app::MAX_STRENGTH * 100.0;
    let mut falloff = radial.falloff;
    let mut falloff_changed = false;

    egui::Grid::new(("spot_sliders", id))
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            if separate {
                for (value, label, rail) in [
                    (&mut edited.r, "Red", Some(egui::Color32::RED)),
                    (&mut edited.g, "Green", Some(egui::Color32::GREEN)),
                    (&mut edited.b, "Blue", Some(egui::Color32::BLUE)),
                ] {
                    ui.label(label);
                    strength_changed |= add_strength_slider(ui, value, limit, rail).changed();
                    ui.end_row();
                }
            } else {
                let mut all = percent.r;
                ui.label("Strength");
                if add_strength_slider(ui, &mut all, limit, None).changed() {
                    edited = Rgb::splat(all);
                    strength_changed = true;
                }
                ui.end_row();
            }

            ui.label("Falloff");
            falloff_changed |= ui
                .add(Slider::new(&mut falloff, 0.2..=4.0).fixed_decimals(2))
                .changed();
            ui.end_row();
        });

    if strength_changed {
        app.edit_defect(id, |d| d.strength = edited * 0.01);
    }
    if falloff_changed {
        app.edit_defect(id, |d| d.falloff = falloff);
    }
    ui.label(
        RichText::new("How much brighter than the rest of the panel this spot is.")
            .small()
            .weak(),
    );
}

fn confirm_delete_dialog(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    let Some(id) = state.confirm_delete else {
        return;
    };
    let name = app.selected_display().and_then(|display| {
        display
            .defects
            .iter()
            .position(|d| d.id() == id)
            .map(config::DisplayConfig::defect_label)
    });
    let Some(name) = name else {
        state.confirm_delete = None;
        return;
    };

    let modal = egui::Modal::new(egui::Id::new("confirm_delete_spot")).show(ui.ctx(), |ui| {
        ui.set_width(360.0);
        ui.heading("Delete this spot?");
        ui.label(format!(
            "Remove {name} from this display? This cannot be undone."
        ));
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            let mut choice = None;
            let cancel = icon_button("Cancel").atom_ui(ui);
            paint_icon(ui, &cancel, BtnIcon::Cancel, None);
            if cancel.response.clicked() {
                choice = Some(false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let delete = icon_button(RichText::new("Delete").color(egui::Color32::WHITE))
                    .fill(egui::Color32::from_rgb(200, 90, 70))
                    .atom_ui(ui);
                paint_icon(ui, &delete, BtnIcon::Delete, Some(egui::Color32::WHITE));
                if delete.response.clicked() {
                    choice = Some(true);
                }
            });
            choice
        })
        .inner
    });

    let confirmed = modal.inner == Some(true);
    let dismissed = modal.should_close() || modal.inner == Some(false);
    if confirmed {
        let was_moving = app.is_editing() && app.selected_defect() == Some(id);
        if state.params_open == Some(id) {
            state.params_open = None;
        }
        app.delete_defect(id);
        if was_moving {
            app.set_editing(false);
        }
        state.confirm_delete = None;
    } else if dismissed {
        state.confirm_delete = None;
    }
}

fn after_reload(state: &mut UiState) {
    state.params_open = None;
    state.confirm_delete = None;
}

fn reload_config(app: &mut App, state: &mut UiState) {
    match app.reload() {
        Ok(()) => {
            after_reload(state);
            state.notice(format!("Loaded {}", app.config_path().display()));
        }
        Err(error) => state.notice(format!("Could not load configuration: {error}")),
    }
}

fn save_config(app: &mut App, state: &mut UiState) {
    match app.save() {
        Ok(()) => {
            state.notice(format!("Saved to {}", app.config_path().display()));
        }
        Err(error) => state.notice(format!("Could not save: {error}")),
    }
}

fn bottom_row(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    ui.horizontal(|ui| {
        let save = icon_button(RichText::new("Save").color(egui::Color32::WHITE))
            .fill(egui::Color32::from_rgb(70, 170, 110))
            .atom_ui(ui);
        paint_icon(ui, &save, BtnIcon::Save, Some(egui::Color32::WHITE));
        if save
            .response
            .on_hover_text("Write the current spots to the configuration file")
            .clicked()
        {
            save_config(app, state);
        }
        let load = icon_button("Reload").atom_ui(ui);
        paint_icon(ui, &load, BtnIcon::Load, None);
        if load
            .response
            .on_hover_text("Discard unsaved edits and load the configuration file from disk")
            .clicked()
        {
            reload_config(app, state);
        }
        if app.unsaved_changes() {
            ui.label(RichText::new("unsaved changes").weak().small());
        }
    });

    ui.horizontal(|ui| {
        let path = app.config_path().display().to_string();
        ui.label(RichText::new(&path).small().weak());
        let copy_size = ui.text_style_height(&egui::TextStyle::Small);
        let copy = egui::Button::new(egui::Atom::custom(
            egui::Id::new(ICON_ATOM),
            Vec2::splat(copy_size),
        ))
        .frame(false)
        .atom_ui(ui);
        let copied = state.path_copied.as_deref() == Some(path.as_str());
        let (icon, color, hover) = if copied {
            (
                BtnIcon::Check,
                Some(egui::Color32::from_rgb(70, 170, 110)),
                "Copied the configuration path",
            )
        } else {
            (BtnIcon::Copy, None, "Copy the configuration path")
        };
        paint_icon(ui, &copy, icon, color);
        if copy.response.on_hover_text(hover).clicked() {
            ui.ctx().copy_text(path.clone());
            state.path_copied = Some(path);
        }
    });

    ui.add_space(6.0);
    let mut autostart = config::autostart_enabled();
    if ui
        .checkbox(&mut autostart, "Start automatically on login")
        .changed()
    {
        match config::set_autostart(autostart) {
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
    });
}

#[derive(Clone, Copy)]
enum BtnIcon {
    Edit,
    Move,
    Clone,
    Delete,
    Add,
    Save,
    Load,
    Copy,
    Check,
    Cancel,
}

fn icon_button<'a>(text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    egui::Button::new((
        egui::Atom::custom(egui::Id::new(ICON_ATOM), Vec2::splat(ICON_SIZE)),
        text.into(),
    ))
}

fn paint_icon(
    ui: &egui::Ui,
    laid_out: &egui::AtomLayoutResponse,
    icon: BtnIcon,
    color: Option<egui::Color32>,
) {
    let Some(rect) = laid_out.rect(egui::Id::new(ICON_ATOM)) else {
        return;
    };
    let color = color.unwrap_or_else(|| ui.style().interact(&laid_out.response).text_color());
    let painter = ui.painter();
    let stroke = egui::Stroke::new(rect.width() * 0.104, color);
    let r = rect.shrink(rect.width() * 0.096);

    match icon {
        BtnIcon::Edit => paint_edit(painter, r, stroke, color),
        BtnIcon::Move => paint_move(painter, r, stroke),
        BtnIcon::Clone => paint_clone(painter, r, stroke),
        BtnIcon::Delete => paint_delete(painter, r, stroke),
        BtnIcon::Add => paint_add(painter, r, stroke),
        BtnIcon::Save => paint_save(painter, r, stroke),
        BtnIcon::Load => paint_load(painter, r, stroke),
        BtnIcon::Copy => paint_copy(painter, r, stroke),
        BtnIcon::Check => paint_check(painter, r, stroke),
        BtnIcon::Cancel => paint_cancel(painter, r, stroke),
    }
}

fn paint_edit(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke, color: egui::Color32) {
    let dir = Vec2::new(1.0, -1.0).normalized();
    let perp = Vec2::new(dir.y, -dir.x);
    let c = r.center();
    let half = r.width() * 0.38;
    let tip_len = r.width() * 0.305;
    let half_w = r.width() * 0.162;
    let eraser = c - dir * half;
    let neck = c + dir * (half - tip_len);
    let tip = c + dir * half;
    painter.add(egui::Shape::closed_line(
        vec![
            eraser + perp * half_w,
            neck + perp * half_w,
            tip,
            neck - perp * half_w,
            eraser - perp * half_w,
        ],
        stroke,
    ));
    painter.line_segment([eraser + perp * half_w, eraser - perp * half_w], stroke);
    painter.circle_filled(tip, r.width() * 0.057, color);
}

fn paint_move(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let c = r.center();
    let arm = r.width() * 0.38;
    let head = r.width() * 0.16;
    painter.line_segment([pos2(c.x, c.y - arm), pos2(c.x, c.y + arm)], stroke);
    painter.line_segment([pos2(c.x - arm, c.y), pos2(c.x + arm, c.y)], stroke);
    painter.line_segment(
        [pos2(c.x, c.y - arm), pos2(c.x - head, c.y - arm + head)],
        stroke,
    );
    painter.line_segment(
        [pos2(c.x, c.y - arm), pos2(c.x + head, c.y - arm + head)],
        stroke,
    );
    painter.line_segment(
        [pos2(c.x, c.y + arm), pos2(c.x - head, c.y + arm - head)],
        stroke,
    );
    painter.line_segment(
        [pos2(c.x, c.y + arm), pos2(c.x + head, c.y + arm - head)],
        stroke,
    );
    painter.line_segment(
        [pos2(c.x - arm, c.y), pos2(c.x - arm + head, c.y - head)],
        stroke,
    );
    painter.line_segment(
        [pos2(c.x - arm, c.y), pos2(c.x - arm + head, c.y + head)],
        stroke,
    );
    painter.line_segment(
        [pos2(c.x + arm, c.y), pos2(c.x + arm - head, c.y - head)],
        stroke,
    );
    painter.line_segment(
        [pos2(c.x + arm, c.y), pos2(c.x + arm - head, c.y + head)],
        stroke,
    );
}

fn paint_clone(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let shift = r.width() * 0.333;
    let back = egui::Rect::from_min_max(
        pos2(r.left(), r.top()),
        pos2(r.right() - shift, r.bottom() - shift),
    );
    let front = egui::Rect::from_min_max(
        pos2(r.left() + shift, r.top() + shift),
        pos2(r.right(), r.bottom()),
    );
    let radius = r.width() * 0.095;
    painter.rect_stroke(back, radius, stroke, StrokeKind::Middle);
    painter.rect_stroke(front, radius, stroke, StrokeKind::Middle);
}

fn paint_delete(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let w = r.width();
    let lid_y = r.top() + w * 0.343;
    let body = egui::Rect::from_min_max(
        pos2(r.left() + w * 0.171, lid_y),
        pos2(r.right() - w * 0.171, r.bottom() - w * 0.038),
    );
    painter.rect_stroke(body, w * 0.114, stroke, StrokeKind::Middle);
    painter.line_segment(
        [
            pos2(r.left() + w * 0.057, lid_y),
            pos2(r.right() - w * 0.057, lid_y),
        ],
        stroke,
    );
    let handle_y = r.top() + w * 0.133;
    let handle_w = w * 0.190;
    let cx = r.center().x;
    painter.line_segment(
        [pos2(cx - handle_w, handle_y), pos2(cx + handle_w, handle_y)],
        stroke,
    );
    painter.line_segment(
        [pos2(cx - handle_w, handle_y), pos2(cx - handle_w, lid_y)],
        stroke,
    );
    painter.line_segment(
        [pos2(cx + handle_w, handle_y), pos2(cx + handle_w, lid_y)],
        stroke,
    );
    painter.line_segment(
        [
            pos2(cx, body.top() + w * 0.210),
            pos2(cx, body.bottom() - w * 0.152),
        ],
        stroke,
    );
}

fn paint_add(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let c = r.center();
    let arm = r.width() * 0.32;
    painter.line_segment([pos2(c.x - arm, c.y), pos2(c.x + arm, c.y)], stroke);
    painter.line_segment([pos2(c.x, c.y - arm), pos2(c.x, c.y + arm)], stroke);
}

fn paint_save(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let c = r.center();
    let w = r.width();
    let shaft_top = r.top() + w * 0.08;
    let shaft_bot = c.y + w * 0.08;
    painter.line_segment([pos2(c.x, shaft_top), pos2(c.x, shaft_bot)], stroke);
    let head = w * 0.20;
    painter.line_segment(
        [
            pos2(c.x, shaft_bot + w * 0.04),
            pos2(c.x - head, shaft_bot - head * 0.6),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(c.x, shaft_bot + w * 0.04),
            pos2(c.x + head, shaft_bot - head * 0.6),
        ],
        stroke,
    );
    let tray_y = r.bottom() - w * 0.14;
    painter.line_segment(
        [
            pos2(r.left() + w * 0.14, tray_y - w * 0.16),
            pos2(r.left() + w * 0.14, tray_y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(r.left() + w * 0.14, tray_y),
            pos2(r.right() - w * 0.14, tray_y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(r.right() - w * 0.14, tray_y),
            pos2(r.right() - w * 0.14, tray_y - w * 0.16),
        ],
        stroke,
    );
}

fn paint_load(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let c = r.center();
    let w = r.width();
    let shaft_top = r.top() + w * 0.10;
    let shaft_bot = c.y + w * 0.16;
    painter.line_segment([pos2(c.x, shaft_top), pos2(c.x, shaft_bot)], stroke);
    let head = w * 0.20;
    painter.line_segment(
        [
            pos2(c.x, shaft_top),
            pos2(c.x - head, shaft_top + head * 0.9),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(c.x, shaft_top),
            pos2(c.x + head, shaft_top + head * 0.9),
        ],
        stroke,
    );
    let tray_y = r.bottom() - w * 0.14;
    painter.line_segment(
        [
            pos2(r.left() + w * 0.14, tray_y - w * 0.16),
            pos2(r.left() + w * 0.14, tray_y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(r.left() + w * 0.14, tray_y),
            pos2(r.right() - w * 0.14, tray_y),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(r.right() - w * 0.14, tray_y),
            pos2(r.right() - w * 0.14, tray_y - w * 0.16),
        ],
        stroke,
    );
}

fn paint_copy(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let w = r.width();
    let clip = egui::Rect::from_min_max(
        pos2(r.center().x - w * 0.18, r.top() + w * 0.04),
        pos2(r.center().x + w * 0.18, r.top() + w * 0.28),
    );
    painter.rect_stroke(clip, w * 0.06, stroke, StrokeKind::Middle);
    let board = egui::Rect::from_min_max(
        pos2(r.left() + w * 0.16, r.top() + w * 0.18),
        pos2(r.right() - w * 0.16, r.bottom() - w * 0.06),
    );
    painter.rect_stroke(board, w * 0.10, stroke, StrokeKind::Middle);
}

fn paint_check(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let w = r.width();
    let stroke = egui::Stroke::new(stroke.width.max(w * 0.14), stroke.color);
    let start = pos2(r.left() + w * 0.16, r.center().y + w * 0.04);
    let mid = pos2(r.center().x - w * 0.04, r.bottom() - w * 0.20);
    let end = pos2(r.right() - w * 0.14, r.top() + w * 0.20);
    painter.line_segment([start, mid], stroke);
    painter.line_segment([mid, end], stroke);
}

fn paint_cancel(painter: &egui::Painter, r: egui::Rect, stroke: egui::Stroke) {
    let inset = r.width() * 0.22;
    painter.line_segment(
        [
            pos2(r.left() + inset, r.top() + inset),
            pos2(r.right() - inset, r.bottom() - inset),
        ],
        stroke,
    );
    painter.line_segment(
        [
            pos2(r.right() - inset, r.top() + inset),
            pos2(r.left() + inset, r.bottom() - inset),
        ],
        stroke,
    );
}

//! The calibration window's layout.

use egui::{pos2, RichText, Slider, StrokeKind, Vec2};
use uuid::Uuid;

use crate::{
    app::App,
    compensation::{MaskQuality, RadialDefect, Rgb},
    config,
    display::DisplayIdentity,
    overlay::ShowMode,
};

use super::{test_pattern, UiState};

/// Space left to the right of a slider's rail for its value box and label.
const SLIDER_LABEL_ROOM: f32 = 230.0;

/// Smallest strength the slider resolves, in percent.
const STRENGTH_FLOOR: f64 = 0.5;

/// Slot reserved for a stroked glyph on the spot-list buttons.
const ICON_ATOM: &str = "spot_btn_icon";
const ICON_SIZE: f32 = 26.0;

/// How much larger the defect-list controls are than the rest of the window.
const DEFECT_CONTROL_SCALE: f32 = 2.0;

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
        defect_list(ui, app, state);
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
}

fn defect_list(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    ui.scope(|ui| {
        enlarge_defect_controls(ui);
        defect_list_inner(ui, app, state);
    });
}

fn enlarge_defect_controls(ui: &mut egui::Ui) {
    let style = ui.style_mut();
    style.spacing.interact_size *= DEFECT_CONTROL_SCALE;
    style.spacing.button_padding *= DEFECT_CONTROL_SCALE;
    style.spacing.icon_width *= DEFECT_CONTROL_SCALE;
    style.spacing.icon_width_inner *= DEFECT_CONTROL_SCALE;
    style.spacing.icon_spacing *= DEFECT_CONTROL_SCALE;
    style.spacing.item_spacing *= DEFECT_CONTROL_SCALE;
    for font in style.text_styles.values_mut() {
        font.size *= DEFECT_CONTROL_SCALE;
    }
}

fn defect_list_inner(ui: &mut egui::Ui, app: &mut App, state: &mut UiState) {
    ui.heading("Compensated Defects");

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

    if entries.is_empty() {
        ui.label(RichText::new("None yet. Add a spot over each blemish.").weak());
    }

    let editing = app.is_editing();
    let active = app.selected_defect();
    let delete_fill = egui::Color32::from_rgb(200, 90, 70);
    let params_open = state.params_open;

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
            ui.label(name);

            let params_this = params_open == Some(*id);
            let edit = icon_button("Edit").selected(params_this).atom_ui(ui);
            paint_spot_icon(ui, &edit, SpotIcon::Edit, None);
            if edit
                .response
                .on_hover_text("Show strength, falloff and rotation for this spot")
                .clicked()
            {
                state.params_open = if params_this { None } else { Some(*id) };
            }

            let moving_this = editing && active == Some(*id);
            let move_btn = icon_button("Move").selected(moving_this).atom_ui(ui);
            paint_spot_icon(ui, &move_btn, SpotIcon::Move, None);
            if move_btn
                .response
                .on_hover_text(
                    "Drag this spot on the screen: wheel to resize, Shift+wheel for \
strength, Esc or a click on empty screen to leave",
                )
                .clicked()
            {
                if moving_this {
                    app.set_editing(false);
                } else {
                    app.select_defect(Some(*id));
                    app.set_editing(true);
                    state.notice(
                        "The overlay is now interactive: drag the spot onto the blemish, \
wheel to resize, Shift+wheel for strength, n for a new spot, Esc or a click on empty screen to leave.",
                    );
                }
            }

            let clone = icon_button("Clone").atom_ui(ui);
            paint_spot_icon(ui, &clone, SpotIcon::Clone, None);
            if clone
                .response
                .on_hover_text("Copy this spot, offset so both stay reachable")
                .clicked()
            {
                app.clone_defect(*id);
                state.notice("Cloned the spot beside the original.");
            }

            let delete = icon_button(RichText::new("Delete").color(egui::Color32::WHITE))
                .fill(delete_fill)
                .atom_ui(ui);
            paint_spot_icon(ui, &delete, SpotIcon::Delete, Some(egui::Color32::WHITE));
            if delete.response.clicked() {
                if state.params_open == Some(*id) {
                    state.params_open = None;
                }
                app.delete_defect(*id);
            }
        });

        if state.params_open == Some(*id) {
            if let Some(radial) = app
                .selected_display()
                .and_then(|d| d.defects.iter().find(|d| d.id() == *id))
                .and_then(|d| d.as_radial())
                .cloned()
            {
                ui.indent(*id, |ui| {
                    ui.group(|ui| {
                        spot_params(ui, app, state, *id, radial);
                    });
                });
            }
        }
    }

    ui.add_space(4.0);
    let add = icon_button("Add spot").atom_ui(ui);
    paint_spot_icon(ui, &add, SpotIcon::Add, None);
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

#[derive(Clone, Copy)]
enum SpotIcon {
    Edit,
    Move,
    Clone,
    Delete,
    Add,
}

fn icon_button<'a>(text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
    egui::Button::new((
        egui::Atom::custom(egui::Id::new(ICON_ATOM), Vec2::splat(ICON_SIZE)),
        text.into(),
    ))
}

fn paint_spot_icon(
    ui: &egui::Ui,
    laid_out: &egui::AtomLayoutResponse,
    icon: SpotIcon,
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
        SpotIcon::Edit => paint_edit(painter, r, stroke, color),
        SpotIcon::Move => paint_move(painter, r, stroke),
        SpotIcon::Clone => paint_clone(painter, r, stroke),
        SpotIcon::Delete => paint_delete(painter, r, stroke),
        SpotIcon::Add => paint_add(painter, r, stroke),
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

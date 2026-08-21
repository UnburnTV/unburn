//! Phosphor Icons for the calibration window.
//!
//! The glyphs are the Regular weight of [Phosphor](https://phosphoricons.com/),
//! registered as a fallback so they render at the same size and colour as the
//! button labels. The bundled font and its MIT licence live in `assets/fonts/`.

use std::sync::Arc;

/// Font definitions with Phosphor Regular as a `Proportional` fallback.
pub fn phosphor_fonts() -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "phosphor".to_owned(),
        Arc::new(egui::FontData::from_static(include_bytes!(
            "../../assets/fonts/Phosphor.ttf"
        ))),
    );
    if let Some(proportional) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
        proportional.insert(1, "phosphor".to_owned());
    }
    fonts
}

/// Install Phosphor so later frames can paint the button glyphs.
pub fn install(ctx: &egui::Context) {
    ctx.set_fonts(phosphor_fonts());
}

#[derive(Clone, Copy)]
pub enum BtnIcon {
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

impl BtnIcon {
    /// Phosphor Regular codepoint for this control.
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Edit => "\u{E3B4}",
            Self::Move => "\u{E0A4}",
            Self::Clone => "\u{E1CA}",
            Self::Delete => "\u{E4A6}",
            Self::Add => "\u{E3D4}",
            Self::Save => "\u{E248}",
            Self::Load => "\u{E036}",
            Self::Copy => "\u{E1CC}",
            Self::Check => "\u{E182}",
            Self::Cancel => "\u{E4F6}",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buttons_use_phosphor_regular_codepoints() {
        assert_eq!(BtnIcon::Edit.glyph(), "\u{E3B4}");
        assert_eq!(BtnIcon::Move.glyph(), "\u{E0A4}");
        assert_eq!(BtnIcon::Clone.glyph(), "\u{E1CA}");
        assert_eq!(BtnIcon::Delete.glyph(), "\u{E4A6}");
        assert_eq!(BtnIcon::Add.glyph(), "\u{E3D4}");
        assert_eq!(BtnIcon::Save.glyph(), "\u{E248}");
        assert_eq!(BtnIcon::Load.glyph(), "\u{E036}");
        assert_eq!(BtnIcon::Copy.glyph(), "\u{E1CC}");
        assert_eq!(BtnIcon::Check.glyph(), "\u{E182}");
        assert_eq!(BtnIcon::Cancel.glyph(), "\u{E4F6}");
    }

    #[test]
    fn phosphor_is_proportional_fallback() {
        let fonts = phosphor_fonts();
        assert!(fonts.font_data.contains_key("phosphor"));
        let proportional = &fonts.families[&egui::FontFamily::Proportional];
        assert_eq!(proportional.get(1).map(String::as_str), Some("phosphor"));
    }
}

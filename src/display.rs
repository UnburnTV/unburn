//! Identifying monitors and describing their geometry.
//!
//! A profile is worthless if it lands on the wrong screen after a reboot, so
//! displays are matched on whatever stable information the platform hands us —
//! never on their position in the desktop layout.

use serde::{Deserialize, Serialize};

use crate::compensation::Vec2;

/// Stable-ish facts about a monitor, in decreasing order of trustworthiness.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayIdentity {
    /// Connector name such as `HDMI-A-1` or `DP-2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    /// Hex digest of the raw EDID block, when the platform exposes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edid_hash: Option<String>,
}

/// Confidence that two identities describe the same physical monitor.
///
/// Anything at or above [`MatchScore::WEAK`] is treated as a match; the caller
/// still picks the highest scoring candidate.
pub struct MatchScore;

impl MatchScore {
    pub const NONE: u32 = 0;
    pub const WEAK: u32 = 10;
}

impl DisplayIdentity {
    /// How strongly `self` and `other` look like the same monitor.
    pub fn match_score(&self, other: &Self) -> u32 {
        fn same(a: &Option<String>, b: &Option<String>) -> bool {
            match (a, b) {
                (Some(a), Some(b)) => !a.is_empty() && a.eq_ignore_ascii_case(b),
                _ => false,
            }
        }

        let mut score = 0;
        if same(&self.edid_hash, &other.edid_hash) {
            score += 100;
        }
        if same(&self.serial, &other.serial) {
            score += 60;
        }
        if same(&self.manufacturer, &other.manufacturer) && same(&self.model, &other.model) {
            score += 25;
        }
        if same(&self.connector, &other.connector) {
            score += 10;
        }
        score
    }

    /// A short label for menus and dropdowns.
    pub fn describe(&self) -> String {
        let connector = self.connector.as_deref().unwrap_or("unknown");
        let product: Vec<&str> = [self.manufacturer.as_deref(), self.model.as_deref()]
            .into_iter()
            .flatten()
            .filter(|s| !s.is_empty())
            .collect();
        if product.is_empty() {
            connector.to_string()
        } else {
            format!("{connector} — {}", product.join(" "))
        }
    }
}

/// How the compositor orients the buffer we hand it relative to the panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Transform {
    #[default]
    Normal,
    Rotate90,
    Rotate180,
    Rotate270,
    Flipped,
    FlippedRotate90,
    FlippedRotate180,
    FlippedRotate270,
}

impl Transform {
    /// Whether the surface's width and height are swapped relative to the panel.
    pub fn swaps_axes(self) -> bool {
        matches!(
            self,
            Transform::Rotate90
                | Transform::Rotate270
                | Transform::FlippedRotate90
                | Transform::FlippedRotate270
        )
    }

    /// The linear part of panel-space → surface-space, as `[[a, b], [c, d]]`
    /// acting on normalized offsets.
    fn linear(self) -> [[f32; 2]; 2] {
        match self {
            Transform::Normal => [[1.0, 0.0], [0.0, 1.0]],
            Transform::Rotate90 => [[0.0, 1.0], [-1.0, 0.0]],
            Transform::Rotate180 => [[-1.0, 0.0], [0.0, -1.0]],
            Transform::Rotate270 => [[0.0, -1.0], [1.0, 0.0]],
            Transform::Flipped => [[-1.0, 0.0], [0.0, 1.0]],
            Transform::FlippedRotate90 => [[0.0, 1.0], [1.0, 0.0]],
            Transform::FlippedRotate180 => [[1.0, 0.0], [0.0, -1.0]],
            Transform::FlippedRotate270 => [[0.0, -1.0], [-1.0, 0.0]],
        }
    }

    /// Map a normalized panel coordinate onto the surface we paint.
    pub fn panel_to_surface(self, uv: Vec2) -> Vec2 {
        let c = Vec2::new(uv.x - 0.5, uv.y - 0.5);
        let m = self.linear();
        Vec2::new(
            m[0][0] * c.x + m[0][1] * c.y + 0.5,
            m[1][0] * c.x + m[1][1] * c.y + 0.5,
        )
    }

    /// Map a normalized offset (a direction, not a point) into surface space.
    pub fn direction_to_surface(self, d: Vec2) -> Vec2 {
        let m = self.linear();
        Vec2::new(m[0][0] * d.x + m[0][1] * d.y, m[1][0] * d.x + m[1][1] * d.y)
    }

    /// Map a normalized surface coordinate back onto the panel.
    pub fn surface_to_panel(self, uv: Vec2) -> Vec2 {
        let c = Vec2::new(uv.x - 0.5, uv.y - 0.5);
        let m = self.linear();
        // Signed permutation matrices are orthogonal, so the inverse is the
        // transpose.
        Vec2::new(
            m[0][0] * c.x + m[1][0] * c.y + 0.5,
            m[0][1] * c.x + m[1][1] * c.y + 0.5,
        )
    }
}

/// Everything a backend knows about one connected output.
#[derive(Debug, Clone, PartialEq)]
pub struct OutputInfo {
    pub id: OutputId,
    pub identity: DisplayIdentity,
    /// Surface size in the coordinate space we must paint, in pixels.
    pub width: u32,
    pub height: u32,
    /// Position in the desktop layout. Used for arranging previews only, never
    /// for identifying the display.
    pub position: (i32, i32),
    pub scale: f64,
    pub transform: Transform,
    /// Refresh rate in millihertz, when known.
    pub refresh_mhz: Option<u32>,
}

impl OutputInfo {
    pub fn aspect(&self) -> f32 {
        if self.height == 0 {
            1.0
        } else {
            self.width as f32 / self.height as f32
        }
    }

    /// Panel-space pixel dimensions, undoing any rotation.
    pub fn panel_size(&self) -> (u32, u32) {
        if self.transform.swaps_axes() {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        }
    }

    pub fn panel_aspect(&self) -> f32 {
        let (w, h) = self.panel_size();
        if h == 0 {
            1.0
        } else {
            w as f32 / h as f32
        }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} ({}×{})",
            self.identity.describe(),
            self.width,
            self.height
        )
    }
}

/// Backend-assigned handle for a connected output. Only meaningful to the
/// backend that produced it, and only until that output disappears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OutputId(pub u32);

/// Backend-assigned handle for a created overlay surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OverlayId(pub u32);

/// Pick the connected output that best matches a stored identity.
pub fn best_match<'a>(
    stored: &DisplayIdentity,
    outputs: impl IntoIterator<Item = &'a OutputInfo>,
) -> Option<&'a OutputInfo> {
    outputs
        .into_iter()
        .map(|o| (o.identity.match_score(stored), o))
        .filter(|(score, _)| *score >= MatchScore::WEAK)
        .max_by_key(|(score, _)| *score)
        .map(|(_, o)| o)
}

/// Read what a monitor says about itself out of its EDID block.
///
/// Only the fields useful for recognising the same panel again are extracted;
/// everything else in EDID is timing information the program never touches.
pub fn identity_from_edid(edid: &[u8]) -> DisplayIdentity {
    let mut identity = DisplayIdentity {
        edid_hash: Some(edid_hash(edid)),
        ..Default::default()
    };
    if edid.len() < 128 {
        return identity;
    }

    // Bytes 8..10 hold three five-bit letters, biased so that 1 means 'A'.
    let packed = u16::from_be_bytes([edid[8], edid[9]]);
    let letters: String = [(packed >> 10) & 0x1f, (packed >> 5) & 0x1f, packed & 0x1f]
        .into_iter()
        .filter(|c| (1..=26).contains(c))
        .map(|c| (b'A' + c as u8 - 1) as char)
        .collect();
    if letters.len() == 3 {
        identity.manufacturer = Some(letters);
    }

    let product = u16::from_le_bytes([edid[10], edid[11]]);
    let serial_number = u32::from_le_bytes([edid[12], edid[13], edid[14], edid[15]]);

    // The four 18-byte descriptors may carry a human-readable name and serial.
    for block in edid[54..126].chunks_exact(18) {
        if block[0..3] != [0, 0, 0] {
            continue;
        }
        let text: String = block[5..18]
            .iter()
            .take_while(|b| **b != 0x0a)
            .map(|b| *b as char)
            .collect::<String>()
            .trim()
            .to_string();
        if text.is_empty() {
            continue;
        }
        match block[3] {
            0xfc => identity.model = Some(text),
            0xff => identity.serial = Some(text),
            _ => {}
        }
    }

    if identity.model.is_none() && product != 0 {
        identity.model = Some(format!("{product:04X}"));
    }
    if identity.serial.is_none() && serial_number != 0 {
        identity.serial = Some(format!("{serial_number:08X}"));
    }
    identity
}

/// Cheap, stable digest of a raw EDID block.
pub fn edid_hash(edid: &[u8]) -> String {
    // FNV-1a: not cryptographic, but EDID blocks are not adversarial and this
    // avoids pulling in a hashing dependency for a display fingerprint.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in edid {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{hash:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ident(connector: &str, model: &str, serial: Option<&str>) -> DisplayIdentity {
        DisplayIdentity {
            connector: Some(connector.into()),
            manufacturer: Some("Samsung".into()),
            model: Some(model.into()),
            serial: serial.map(Into::into),
            edid_hash: None,
        }
    }

    fn output(id: u32, identity: DisplayIdentity) -> OutputInfo {
        OutputInfo {
            id: OutputId(id),
            identity,
            width: 3840,
            height: 2160,
            position: (0, 0),
            scale: 1.0,
            transform: Transform::Normal,
            refresh_mhz: Some(60_000),
        }
    }

    #[test]
    fn edid_beats_everything_else() {
        let mut a = ident("HDMI-A-1", "QN90", Some("ABC"));
        a.edid_hash = Some("deadbeef".into());
        let mut b = ident("DP-1", "Other", Some("XYZ"));
        b.edid_hash = Some("deadbeef".into());
        assert!(a.match_score(&b) >= 100);
    }

    #[test]
    fn a_replugged_monitor_on_another_port_still_matches() {
        let stored = ident("HDMI-A-1", "QN90", Some("ABC"));
        let moved = ident("HDMI-A-2", "QN90", Some("ABC"));
        assert!(stored.match_score(&moved) >= MatchScore::WEAK);
    }

    #[test]
    fn different_monitors_do_not_match() {
        let a = DisplayIdentity {
            connector: Some("HDMI-A-1".into()),
            ..Default::default()
        };
        let b = DisplayIdentity {
            connector: Some("DP-3".into()),
            ..Default::default()
        };
        assert_eq!(a.match_score(&b), MatchScore::NONE);
    }

    #[test]
    fn best_match_prefers_the_strongest_evidence() {
        let stored = ident("HDMI-A-1", "QN90", Some("ABC"));
        let outputs = vec![
            output(1, ident("HDMI-A-1", "Other", None)),
            output(2, ident("DP-1", "QN90", Some("ABC"))),
        ];
        let found = best_match(&stored, &outputs).unwrap();
        assert_eq!(found.id, OutputId(2));
    }

    #[test]
    fn no_candidate_means_no_match() {
        let stored = ident("HDMI-A-1", "QN90", Some("ABC"));
        assert!(best_match(&stored, &[]).is_none());
    }

    #[test]
    fn transforms_round_trip() {
        let point = Vec2::new(0.63, 0.41);
        for t in [
            Transform::Normal,
            Transform::Rotate90,
            Transform::Rotate180,
            Transform::Rotate270,
            Transform::Flipped,
            Transform::FlippedRotate90,
            Transform::FlippedRotate180,
            Transform::FlippedRotate270,
        ] {
            let back = t.surface_to_panel(t.panel_to_surface(point));
            assert!((back.x - point.x).abs() < 1e-5, "{t:?}");
            assert!((back.y - point.y).abs() < 1e-5, "{t:?}");
        }
    }

    #[test]
    fn quarter_turn_moves_the_top_left_corner_to_the_bottom_left() {
        let corner = Vec2::new(0.0, 0.0);
        let moved = Transform::Rotate90.panel_to_surface(corner);
        assert!((moved.x - 0.0).abs() < 1e-5);
        assert!((moved.y - 1.0).abs() < 1e-5);
    }

    #[test]
    fn panel_size_undoes_rotation() {
        let mut o = output(1, ident("HDMI-A-1", "QN90", None));
        o.width = 1080;
        o.height = 1920;
        o.transform = Transform::Rotate90;
        assert_eq!(o.panel_size(), (1920, 1080));
    }

    #[test]
    fn edid_hash_is_stable_and_discriminating() {
        assert_eq!(edid_hash(b"abc"), edid_hash(b"abc"));
        assert_ne!(edid_hash(b"abc"), edid_hash(b"abd"));
    }

    /// A synthetic but structurally valid EDID block.
    fn sample_edid() -> Vec<u8> {
        let mut edid = vec![0u8; 128];
        edid[0..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        // "SAM" packed into the manufacturer field.
        let packed: u16 = (19 << 10) | (1 << 5) | 13;
        edid[8..10].copy_from_slice(&packed.to_be_bytes());
        edid[10..12].copy_from_slice(&0x0f1eu16.to_le_bytes());
        edid[12..16].copy_from_slice(&0x0001_e240u32.to_le_bytes());

        let mut descriptor = |offset: usize, tag: u8, text: &str| {
            edid[offset..offset + 3].copy_from_slice(&[0, 0, 0]);
            edid[offset + 3] = tag;
            edid[offset + 4] = 0;
            let bytes = text.as_bytes();
            edid[offset + 5..offset + 5 + bytes.len()].copy_from_slice(bytes);
            edid[offset + 5 + bytes.len()] = 0x0a;
        };
        descriptor(72, 0xfc, "QN90B");
        descriptor(90, 0xff, "SN12345");
        edid
    }

    #[test]
    fn edid_yields_a_usable_identity() {
        let identity = identity_from_edid(&sample_edid());
        assert_eq!(identity.manufacturer.as_deref(), Some("SAM"));
        assert_eq!(identity.model.as_deref(), Some("QN90B"));
        assert_eq!(identity.serial.as_deref(), Some("SN12345"));
        assert!(identity.edid_hash.is_some());
    }

    #[test]
    fn edid_without_descriptors_falls_back_to_the_numeric_fields() {
        let mut edid = sample_edid();
        edid[54..126].fill(0);
        let identity = identity_from_edid(&edid);
        assert_eq!(identity.model.as_deref(), Some("0F1E"));
        assert_eq!(identity.serial.as_deref(), Some("0001E240"));
    }

    #[test]
    fn a_truncated_edid_still_gives_a_hash() {
        let identity = identity_from_edid(&[1, 2, 3]);
        assert!(identity.edid_hash.is_some());
        assert!(identity.model.is_none());
    }
}

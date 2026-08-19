//! Asking the desktop who a monitor actually is.
//!
//! Compensation is traced against one physical panel's wear, so applying it to
//! the wrong screen is the worst thing this program can do. That makes a stable
//! per-unit identifier — a serial number, or a digest of the whole EDID block —
//! the difference between recognising a display and guessing at it. Neither
//! `wl_output` nor XRandR-under-XWayland carries one: the Wayland protocol has
//! no field for a serial, and XWayland exposes no EDID property at all.
//!
//! # Why not read the kernel's EDID files
//!
//! Linux already publishes the full EDID of every connected monitor at
//! `/sys/class/drm/<card>-<connector>/edid`, world-readable and independent of
//! any display server. It is tempting, and it was rejected.
//!
//! The obstacle is that those files are keyed by the *kernel's* connector name
//! while an overlay is placed on the *display server's* output, and the two
//! disagree: the kernel spells the HDMI variant letter, as in `HDMI-A-1`, while
//! Mutter and Xorg both drop it and say `HDMI-1`. Nothing in either enumeration
//! points at the other — there is no shared connector id — so joining them means
//! translating names between two projects' spelling conventions. Matching on
//! make and model instead cannot rescue it, because that is ambiguous in exactly
//! the case this module exists to solve: two monitors of the same model.
//!
//! Asking each desktop through its own interface avoids the problem outright.
//! Every source here reports output names produced by the same compositor that
//! named the outputs unburn draws on, so the join is plain string equality with
//! no translation table to keep correct. The price is one interface per desktop,
//! which is the trade this module accepts.
//!
//! # What each desktop can tell us
//!
//! | Desktop | Interface | Best identity |
//! |---|---|---|
//! | GNOME | `org.gnome.Mutter.DisplayConfig` over D-Bus | serial number |
//! | wlroots (sway, Hyprland, river) | `zwlr_output_manager_v1` | serial number |
//! | KDE Plasma | `kde_output_device_v2` | the whole EDID block |
//!
//! Only Plasma hands over raw EDID, so only there does a display reach a full
//! fingerprint; elsewhere a serial number is enough to tell two units of one
//! model apart, which is what matters.

use std::collections::HashMap;

use tracing::debug;

use crate::display::{DisplayIdentity, OutputInfo};

mod gnome;
mod kde;
mod wlr;

/// Extra facts about the connected monitors, from an interface outside the
/// ordinary display-server protocol.
///
/// Implementations are queried afresh on every monitor change rather than
/// cached, because a serial number is only useful if it belongs to the panel
/// plugged in right now.
pub trait IdentitySource {
    /// For logs, so it is obvious which interface a session ended up using.
    fn label(&self) -> &'static str;

    /// What this desktop knows, keyed by the output name the display server
    /// uses. Names are passed through untouched: they are only ever compared
    /// against names from the same compositor.
    ///
    /// A field must be left unset rather than filled with a stand-in. Only the
    /// implementation knows how its desktop spells "no serial number", so each
    /// one drops its own placeholders here; everything past this point treats
    /// whatever arrives as a real identifier.
    fn identities(&mut self) -> Vec<(String, DisplayIdentity)>;
}

/// Find the interface this session offers, if any.
///
/// The three are mutually exclusive in practice — no compositor implements more
/// than one — so the first that answers wins and the order is not significant.
/// A session with none, such as a nested compositor or a remote desktop, simply
/// goes without and identifies monitors exactly as before.
pub fn detect() -> Option<Box<dyn IdentitySource>> {
    if let Some(source) = gnome::Gnome::connect() {
        return Some(Box::new(source));
    }
    if let Some(source) = wlr::Wlr::connect() {
        return Some(Box::new(source));
    }
    if let Some(source) = kde::Kde::connect() {
        return Some(Box::new(source));
    }
    debug!("no monitor identity interface in this session");
    None
}

/// Fold what `source` knows into `outputs`, matching on output name.
///
/// Anything the display server already reported is left alone; see
/// [`DisplayIdentity::fill_gaps_from`] for why that precedence matters.
pub fn enrich(outputs: &mut [OutputInfo], source: &mut dyn IdentitySource) {
    let extra: HashMap<String, DisplayIdentity> = source.identities().into_iter().collect();
    if extra.is_empty() {
        return;
    }

    for output in outputs.iter_mut() {
        let Some(name) = output.identity.connector.clone() else {
            continue;
        };
        match extra.get(&name) {
            Some(found) => {
                output.identity.fill_gaps_from(found);
                debug!(
                    output = %name,
                    source = source.label(),
                    serial = ?output.identity.serial,
                    "monitor identity filled in"
                );
            }
            None => debug!(
                output = %name,
                source = source.label(),
                "no identity reported for this output"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{OutputId, Transform};

    struct Fake(Vec<(String, DisplayIdentity)>);

    impl IdentitySource for Fake {
        fn label(&self) -> &'static str {
            "fake"
        }
        fn identities(&mut self) -> Vec<(String, DisplayIdentity)> {
            self.0.clone()
        }
    }

    fn output(connector: &str, model: Option<&str>) -> OutputInfo {
        OutputInfo {
            id: OutputId(1),
            identity: DisplayIdentity {
                connector: Some(connector.into()),
                model: model.map(Into::into),
                ..Default::default()
            },
            width: 2560,
            height: 1440,
            position: (0, 0),
            scale: 1.0,
            transform: Transform::Normal,
            refresh_mhz: None,
        }
    }

    fn with_serial(serial: &str) -> DisplayIdentity {
        DisplayIdentity {
            serial: Some(serial.into()),
            ..Default::default()
        }
    }

    #[test]
    fn a_serial_is_attached_to_the_matching_output() {
        let mut outputs = vec![output("HDMI-1", Some("DELL U2722DE"))];
        let mut source = Fake(vec![("HDMI-1".into(), with_serial("31HKFH3"))]);
        enrich(&mut outputs, &mut source);
        assert_eq!(outputs[0].identity.serial.as_deref(), Some("31HKFH3"));
    }

    /// The names come from the same compositor, so no near-miss should ever be
    /// treated as a hit. Guessing here would attach one panel's wear to another.
    #[test]
    fn a_name_that_does_not_match_exactly_is_not_used() {
        let mut outputs = vec![output("HDMI-1", None)];
        let mut source = Fake(vec![("HDMI-A-1".into(), with_serial("31HKFH3"))]);
        enrich(&mut outputs, &mut source);
        assert!(outputs[0].identity.serial.is_none());
    }

    #[test]
    fn each_output_gets_only_its_own_identity() {
        let mut outputs = vec![
            output("HDMI-1", None),
            OutputInfo {
                id: OutputId(2),
                ..output("DP-2", None)
            },
        ];
        let mut source = Fake(vec![
            ("DP-2".into(), with_serial("SECOND")),
            ("HDMI-1".into(), with_serial("FIRST")),
        ]);
        enrich(&mut outputs, &mut source);
        assert_eq!(outputs[0].identity.serial.as_deref(), Some("FIRST"));
        assert_eq!(outputs[1].identity.serial.as_deref(), Some("SECOND"));
    }

    #[test]
    fn what_the_display_server_reported_is_never_replaced() {
        let mut outputs = vec![output("eDP-1", Some("0x0035"))];
        let mut source = Fake(vec![(
            "eDP-1".into(),
            DisplayIdentity {
                model: Some("0035".into()),
                edid_hash: Some("082b8ed09bcc1d35".into()),
                ..Default::default()
            },
        )]);
        enrich(&mut outputs, &mut source);
        assert_eq!(outputs[0].identity.model.as_deref(), Some("0x0035"));
        assert_eq!(
            outputs[0].identity.edid_hash.as_deref(),
            Some("082b8ed09bcc1d35")
        );
    }
}

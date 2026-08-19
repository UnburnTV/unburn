//! GNOME, through Mutter's display configuration service on the session bus.
//!
//! Mutter implements neither of the Wayland output-management protocols the
//! other compositors offer, so D-Bus is the only way to ask it anything. The
//! service is what the Settings panel itself uses.
//!
//! Two things make this worth the dependency. The connector strings it returns
//! are the same ones `wl_output` reports, so outputs join by plain equality. And
//! because it is the compositor answering rather than the display-server
//! protocol, it works just as well for the X11 backend running under XWayland —
//! which on GNOME is the backend that actually gets used, since GNOME has no
//! layer-shell for a real overlay.

use std::collections::HashMap;

use tracing::{debug, warn};
use zbus::{blocking::Connection, zvariant::OwnedValue};

use crate::display::DisplayIdentity;

use super::IdentitySource;

const SERVICE: &str = "org.gnome.Mutter.DisplayConfig";
const PATH: &str = "/org/gnome/Mutter/DisplayConfig";
const METHOD: &str = "GetCurrentState";

/// `(connector, vendor, product, serial)` — Mutter's description of one monitor.
type MonitorSpec = (String, String, String, String);

/// `(id, width, height, refresh, preferred_scale, supported_scales, properties)`
type Mode = (
    String,
    i32,
    i32,
    f64,
    f64,
    Vec<f64>,
    HashMap<String, OwnedValue>,
);

type Monitor = (MonitorSpec, Vec<Mode>, HashMap<String, OwnedValue>);

/// `(x, y, scale, transform, primary, monitors, properties)`
type LogicalMonitor = (
    i32,
    i32,
    f64,
    u32,
    bool,
    Vec<MonitorSpec>,
    HashMap<String, OwnedValue>,
);

/// The whole `GetCurrentState` reply, whose signature is
/// `u a((ssss)a(siiddada{sv})a{sv}) a(iiduba(ssss)a{sv}) a{sv}`.
///
/// Only the monitors are wanted, but D-Bus replies are positional, so the rest
/// has to be described to get past it. This is not a stable public API and
/// GNOME may reshape it; a reply that no longer fits is reported once and then
/// treated as no answer, which costs the serial numbers and nothing else.
type CurrentState = (
    u32,
    Vec<Monitor>,
    Vec<LogicalMonitor>,
    HashMap<String, OwnedValue>,
);

pub struct Gnome {
    connection: Connection,
}

impl Gnome {
    pub fn connect() -> Option<Self> {
        let connection = Connection::session().ok()?;
        let source = Gnome { connection };
        // Nothing but Mutter answers on this name, so a successful call doubles
        // as the test for whether this is a GNOME session at all.
        source.state()?;
        debug!("monitor identity comes from Mutter's display configuration");
        Some(source)
    }

    fn state(&self) -> Option<CurrentState> {
        let reply = self
            .connection
            .call_method(Some(SERVICE), PATH, Some(SERVICE), METHOD, &())
            .ok()?;
        match reply.body().deserialize::<CurrentState>() {
            Ok(state) => Some(state),
            Err(error) => {
                warn!(%error, "Mutter described its monitors in an unexpected shape");
                None
            }
        }
    }
}

impl IdentitySource for Gnome {
    fn label(&self) -> &'static str {
        "Mutter"
    }

    fn identities(&mut self) -> Vec<(String, DisplayIdentity)> {
        let Some((_config_serial, monitors, ..)) = self.state() else {
            return Vec::new();
        };
        monitors
            .into_iter()
            .map(|((connector, vendor, product, serial), ..)| {
                let identity = DisplayIdentity {
                    connector: Some(connector.clone()),
                    manufacturer: text(vendor),
                    model: text(product),
                    serial: real_serial(serial),
                    // This interface reports EDID fields, never the block
                    // itself, so no fingerprint is available on GNOME.
                    edid_hash: None,
                };
                (connector, identity)
            })
            .collect()
    }
}

fn text(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

/// Reject Mutter's way of saying a panel has no serial at all.
///
/// It reports `0x00000000`, which is what most built-in laptop screens come
/// back with. Kept as-is it would be stored as an identifier, and two unrelated
/// serial-less panels would then agree on it — the very false match this module
/// exists to prevent.
///
/// The rule lives here rather than in the desktop-neutral layer because it is
/// Mutter's spelling of "unknown", not a general truth: the wlroots protocol
/// omits the event entirely when there is no serial, and Plasma's field is
/// simply empty.
fn real_serial(value: String) -> Option<String> {
    let digits = value.strip_prefix("0x").unwrap_or(&value);
    if digits.is_empty() || digits.chars().all(|c| c == '0') {
        return None;
    }
    Some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placeholder_serial_is_not_reported() {
        assert_eq!(real_serial("0x00000000".into()), None);
        assert_eq!(real_serial("00000000".into()), None);
        assert_eq!(real_serial("0".into()), None);
        assert_eq!(real_serial(String::new()), None);
    }

    #[test]
    fn a_serial_that_merely_begins_with_a_zero_survives() {
        assert_eq!(real_serial("31HKFH3".into()).as_deref(), Some("31HKFH3"));
        assert_eq!(real_serial("0x0035".into()).as_deref(), Some("0x0035"));
        assert_eq!(real_serial("0001".into()).as_deref(), Some("0001"));
    }
}

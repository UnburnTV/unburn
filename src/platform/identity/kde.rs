//! KDE Plasma, through `kde_output_device_v2`.
//!
//! Plasma is the only desktop that hands over the EDID block
//! itself rather than a few fields parsed out of it, which is why the identity
//! obtained here is the strongest available anywhere: the same fingerprint the
//! X11 backend computes from the `EDID` output property, so a profile written
//! under X11 keeps matching under Plasma's Wayland session and the other way
//! round.
//!
//! Unlike the wlroots protocol there is no single manager object; the compositor
//! advertises one global per output, each of which describes itself and ends
//! with a `done`. Collection therefore waits for every bound device to finish
//! rather than for one overall event.

use std::collections::HashMap;

use base64::Engine;
use tracing::{debug, warn};
use wayland_client::{
    backend::ObjectId, event_created_child, protocol::wl_registry, Connection, Dispatch, Proxy,
    QueueHandle,
};
use wayland_protocols_plasma::output_device::v2::client::{
    kde_output_device_mode_v2::{self, KdeOutputDeviceModeV2},
    kde_output_device_v2::{self, KdeOutputDeviceV2},
};

use crate::display::{identity_from_edid, DisplayIdentity};

use super::IdentitySource;

/// Every device is required to end its initial burst with a `done`. The cap only
/// exists so one that never does cannot wedge a monitor refresh.
const MAX_ROUNDTRIPS: usize = 8;

pub struct Kde;

impl Kde {
    pub fn connect() -> Option<Self> {
        // As with the wlroots source there is no cheaper probe than doing the
        // work once and seeing whether any device answered.
        let devices = collect()?;
        debug!(
            devices = devices.len(),
            "monitor identity comes from kde-output-device"
        );
        Some(Kde)
    }
}

impl IdentitySource for Kde {
    fn label(&self) -> &'static str {
        "kde-output-device"
    }

    fn identities(&mut self) -> Vec<(String, DisplayIdentity)> {
        collect().unwrap_or_default()
    }
}

/// What one output device told us about itself.
#[derive(Default)]
struct Device {
    name: Option<String>,
    make: Option<String>,
    model: Option<String>,
    serial: Option<String>,
    edid: Option<String>,
    done: bool,
}

#[derive(Default)]
struct Collector {
    devices: HashMap<ObjectId, Device>,
    /// Whether any device global was found at all, which is what decides
    /// whether this is a Plasma session.
    bound_any: bool,
}

fn collect() -> Option<Vec<(String, DisplayIdentity)>> {
    let connection = Connection::connect_to_env().ok()?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());

    let mut state = Collector::default();
    queue.roundtrip(&mut state).ok()?;
    if !state.bound_any {
        return None;
    }

    for _ in 0..MAX_ROUNDTRIPS {
        if state.devices.values().all(|device| device.done) {
            break;
        }
        queue.roundtrip(&mut state).ok()?;
    }

    Some(
        state
            .devices
            .into_values()
            .filter_map(|device| {
                let name = device.name.clone()?;
                let identity = identity_of(&device);
                Some((name, identity))
            })
            .collect(),
    )
}

/// Prefer the EDID block, and fall back on the separate fields when it is
/// missing or unreadable — a virtual output has no EDID, and neither does a
/// display behind some adapters.
fn identity_of(device: &Device) -> DisplayIdentity {
    let mut identity = device
        .edid
        .as_deref()
        .and_then(decode_edid)
        .map(|edid| identity_from_edid(&edid))
        .unwrap_or_default();

    identity.connector = device.name.clone();
    // The device's own fields fill whatever the EDID did not carry. They are
    // subordinate because a fingerprint and the strings parsed alongside it
    // describe the same block, and mixing sources would risk a model string
    // that disagrees with the one every other backend derives.
    identity.fill_gaps_from(&DisplayIdentity {
        manufacturer: device.make.clone(),
        model: device.model.clone(),
        serial: device.serial.clone(),
        ..Default::default()
    });
    identity
}

/// Plasma sends the EDID base64-encoded, the one place in this module where a
/// value needs decoding rather than just copying.
fn decode_edid(encoded: &str) -> Option<Vec<u8>> {
    if encoded.is_empty() {
        return None;
    }
    match base64::engine::general_purpose::STANDARD.decode(encoded) {
        Ok(bytes) => Some(bytes),
        Err(error) => {
            warn!(%error, "an output's EDID was not valid base64");
            None
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for Collector {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global {
            name,
            interface,
            version,
        } = event
        else {
            return;
        };
        if interface != KdeOutputDeviceV2::interface().name {
            return;
        }
        // Never bind above what these bindings were generated for, or the
        // compositor may send events this build cannot parse.
        let version = version.min(KdeOutputDeviceV2::interface().version);
        let device = registry.bind::<KdeOutputDeviceV2, _, _>(name, version, handle, ());
        state.devices.entry(device.id()).or_default();
        state.bound_any = true;
    }
}

impl Dispatch<KdeOutputDeviceV2, ()> for Collector {
    fn event(
        state: &mut Self,
        device: &KdeOutputDeviceV2,
        event: kde_output_device_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let entry = state.devices.entry(device.id()).or_default();
        match event {
            kde_output_device_v2::Event::Name { name } => entry.name = Some(name),
            kde_output_device_v2::Event::Geometry { make, model, .. } => {
                entry.make = Some(make);
                entry.model = Some(model);
            }
            kde_output_device_v2::Event::SerialNumber { serialNumber } => {
                entry.serial = Some(serialNumber)
            }
            kde_output_device_v2::Event::Edid { raw } => entry.edid = Some(raw),
            kde_output_device_v2::Event::Done => entry.done = true,
            _ => {}
        }
    }

    // Devices announce their modes as new objects. Nothing here needs a mode,
    // but the child interface still has to be declared or the event cannot be
    // parsed at all.
    event_created_child!(Collector, KdeOutputDeviceV2, [
        kde_output_device_v2::EVT_MODE_OPCODE => (KdeOutputDeviceModeV2, ()),
    ]);
}

impl Dispatch<KdeOutputDeviceModeV2, ()> for Collector {
    fn event(
        _: &mut Self,
        _: &KdeOutputDeviceModeV2,
        _: kde_output_device_mode_v2::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal EDID, enough to check that the block is preferred over the
    /// separate fields: manufacturer letters at bytes 8..10 and a model string
    /// in a descriptor would both come from the block, not from `geometry`.
    fn encoded_edid() -> String {
        let mut edid = vec![0u8; 128];
        edid[..8].copy_from_slice(&[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x00]);
        // 'D', 'E', 'L' packed five bits apiece, biased so 1 means 'A'.
        let packed: u16 = (4 << 10) | (5 << 5) | 12;
        edid[8..10].copy_from_slice(&packed.to_be_bytes());
        base64::engine::general_purpose::STANDARD.encode(&edid)
    }

    #[test]
    fn the_edid_block_is_preferred_over_the_separate_fields() {
        let identity = identity_of(&Device {
            name: Some("DP-1".into()),
            make: Some("Dell Inc.".into()),
            model: Some("U2722DE".into()),
            serial: Some("31HKFH3".into()),
            edid: Some(encoded_edid()),
            done: true,
        });
        assert_eq!(identity.connector.as_deref(), Some("DP-1"));
        assert_eq!(identity.manufacturer.as_deref(), Some("DEL"));
        assert!(
            identity.edid_hash.is_some(),
            "the fingerprint is the whole point of reading the block"
        );
        // Not in the block, so the device's own field is used.
        assert_eq!(identity.serial.as_deref(), Some("31HKFH3"));
    }

    #[test]
    fn an_output_without_an_edid_still_gets_what_the_device_reported() {
        let identity = identity_of(&Device {
            name: Some("DP-1".into()),
            make: Some("Dell Inc.".into()),
            model: Some("U2722DE".into()),
            serial: Some("31HKFH3".into()),
            edid: None,
            done: true,
        });
        assert_eq!(identity.manufacturer.as_deref(), Some("Dell Inc."));
        assert_eq!(identity.model.as_deref(), Some("U2722DE"));
        assert!(identity.edid_hash.is_none());
    }

    #[test]
    fn a_corrupt_edid_does_not_lose_the_rest() {
        let identity = identity_of(&Device {
            name: Some("DP-1".into()),
            serial: Some("31HKFH3".into()),
            edid: Some("not base64 at all!!".into()),
            ..Default::default()
        });
        assert_eq!(identity.serial.as_deref(), Some("31HKFH3"));
    }
}

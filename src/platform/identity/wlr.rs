//! wlroots compositors — sway, Hyprland, river, Wayfire — through
//! `zwlr_output_manager_v1`.
//!
//! The protocol's own wording explains why it is the right place to ask: the
//! make, model and serial number events exist so that "clients can recognize
//! heads from previous sessions and for example load head-specific
//! configurations back". That is precisely what a compensation profile is.
//!
//! Version 2 is the minimum, because that is where the serial number appears. A
//! compositor offering only version 1 is treated as having no identity source at
//! all rather than reporting a make and model that cannot separate two units of
//! one model.

use std::collections::HashMap;

use tracing::debug;
use wayland_client::{
    backend::ObjectId, event_created_child, protocol::wl_registry, Connection, Dispatch, Proxy,
    QueueHandle,
};
use wayland_protocols_wlr::output_management::v1::client::{
    zwlr_output_head_v1::{self, ZwlrOutputHeadV1},
    zwlr_output_manager_v1::{self, ZwlrOutputManagerV1},
    zwlr_output_mode_v1::{self, ZwlrOutputModeV1},
};

use crate::display::DisplayIdentity;

use super::IdentitySource;

/// The serial number event was added in version 2, and without it this source
/// has nothing the ordinary `wl_output` interface does not already provide.
const REQUIRED_VERSION: u32 = 2;

/// A compositor is required to finish its initial burst with a `done`. The cap
/// only exists so one that never does cannot wedge a monitor refresh.
const MAX_ROUNDTRIPS: usize = 8;

pub struct Wlr;

impl Wlr {
    pub fn connect() -> Option<Self> {
        // There is no cheaper probe than doing the work once: the global has to
        // be bound and its heads described before it is known whether this
        // compositor offers a serial number.
        let heads = collect()?;
        debug!(
            heads = heads.len(),
            "monitor identity comes from wlr-output-management"
        );
        Some(Wlr)
    }
}

impl IdentitySource for Wlr {
    fn label(&self) -> &'static str {
        "wlr-output-management"
    }

    fn identities(&mut self) -> Vec<(String, DisplayIdentity)> {
        collect().unwrap_or_default()
    }
}

/// What one head told us about itself.
#[derive(Default)]
struct Head {
    name: Option<String>,
    make: Option<String>,
    model: Option<String>,
    serial: Option<String>,
}

#[derive(Default)]
struct Collector {
    manager: Option<ZwlrOutputManagerV1>,
    heads: HashMap<ObjectId, Head>,
    done: bool,
}

/// Open a short-lived connection of our own, describe every head, and close it.
///
/// A separate connection rather than the overlay backend's: this runs while the
/// backend is mid-refresh, and borrowing its event queue would mean interleaving
/// two unrelated protocols in one dispatch loop for no benefit. It also keeps
/// this source usable from the X11 backend.
fn collect() -> Option<Vec<(String, DisplayIdentity)>> {
    let connection = Connection::connect_to_env().ok()?;
    let mut queue = connection.new_event_queue();
    let handle = queue.handle();
    connection.display().get_registry(&handle, ());

    let mut state = Collector::default();
    // The first roundtrip advertises the globals, which is when the manager is
    // bound; the ones after it carry the heads and their fields.
    queue.roundtrip(&mut state).ok()?;
    state.manager.as_ref()?;

    for _ in 0..MAX_ROUNDTRIPS {
        if state.done {
            break;
        }
        queue.roundtrip(&mut state).ok()?;
    }

    Some(
        state
            .heads
            .into_values()
            .filter_map(|head| {
                let name = head.name?;
                let identity = DisplayIdentity {
                    connector: Some(name.clone()),
                    manufacturer: head.make,
                    model: head.model,
                    serial: head.serial,
                    // The protocol carries EDID fields, never the block itself.
                    edid_hash: None,
                };
                Some((name, identity))
            })
            .collect(),
    )
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
        if interface != ZwlrOutputManagerV1::interface().name || state.manager.is_some() {
            return;
        }
        if version < REQUIRED_VERSION {
            debug!(
                version,
                "wlr-output-management is too old to report serials"
            );
            return;
        }
        // Never bind above what these bindings were generated for, or the
        // compositor may send events this build cannot parse.
        let version = version.min(ZwlrOutputManagerV1::interface().version);
        state.manager = Some(registry.bind::<ZwlrOutputManagerV1, _, _>(name, version, handle, ()));
    }
}

impl Dispatch<ZwlrOutputManagerV1, ()> for Collector {
    fn event(
        state: &mut Self,
        _: &ZwlrOutputManagerV1,
        event: zwlr_output_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // The head object arrives already dispatched to us; its own events
            // carry the fields we are after.
            zwlr_output_manager_v1::Event::Head { head } => {
                state.heads.entry(head.id()).or_default();
            }
            zwlr_output_manager_v1::Event::Done { .. }
            | zwlr_output_manager_v1::Event::Finished => state.done = true,
            _ => {}
        }
    }

    event_created_child!(Collector, ZwlrOutputManagerV1, [
        zwlr_output_manager_v1::EVT_HEAD_OPCODE => (ZwlrOutputHeadV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputHeadV1, ()> for Collector {
    fn event(
        state: &mut Self,
        head: &ZwlrOutputHeadV1,
        event: zwlr_output_head_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let entry = state.heads.entry(head.id()).or_default();
        match event {
            zwlr_output_head_v1::Event::Name { name } => entry.name = Some(name),
            zwlr_output_head_v1::Event::Make { make } => entry.make = Some(make),
            zwlr_output_head_v1::Event::Model { model } => entry.model = Some(model),
            zwlr_output_head_v1::Event::SerialNumber { serial_number } => {
                entry.serial = Some(serial_number)
            }
            _ => {}
        }
    }

    // Heads announce their modes as new objects. Nothing here needs a mode, but
    // the child interface still has to be declared or the event cannot be
    // parsed at all.
    event_created_child!(Collector, ZwlrOutputHeadV1, [
        zwlr_output_head_v1::EVT_MODE_OPCODE => (ZwlrOutputModeV1, ()),
    ]);
}

impl Dispatch<ZwlrOutputModeV1, ()> for Collector {
    fn event(
        _: &mut Self,
        _: &ZwlrOutputModeV1,
        _: zwlr_output_mode_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

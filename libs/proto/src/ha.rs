//! Home Assistant state as the dashboard sees it.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub id: String,
    pub state: String,
    pub name: String,
    pub unit: String,
    /// Light brightness 0..=255, if reported.
    pub brightness: Option<u8>,
    /// Light supports brightness (any color mode other than on/off).
    pub dimmable: bool,
    /// Climate: measured and target temperature.
    pub current_temp: Option<f64>,
    pub target_temp: Option<f64>,
    pub temp_step: f64,
    /// Cover position 0..=100.
    pub position: Option<u8>,
}

impl Entity {
    pub fn domain(&self) -> &str {
        self.id.split('.').next().unwrap_or("")
    }
    pub fn is_on(&self) -> bool {
        matches!(
            self.state.as_str(),
            "on" | "open" | "playing" | "home" | "unlocked"
        )
    }
}

/// Everything the dashboard needs, published by the HA client.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub entities: BTreeMap<String, Entity>,
    pub connected: bool,
    /// Bumped on every change.
    pub version: u64,
    /// Areas (rooms): (area id, name), sorted by name.
    pub areas: Vec<(String, String)>,
    /// entity id -> area id (the entity's own area, else its device's).
    pub entity_area: BTreeMap<String, String>,
    /// Entities hidden in HA, or config/diagnostic entities.
    pub hidden: BTreeSet<String>,
    /// Bumped when the registries (areas/devices/entities) have been loaded.
    pub registry_version: u64,
}

/// Bytes of the board that carries the `Snapshot` (ha writes, the
/// dashboard and the web server read; init makes it). Room for some
/// thousand entities.
pub const STATE_BYTES: u64 = 2 << 20;

/// The service-call ring's geometry: init sizes the memory with these, the
/// dashboard formats it and produces, ha consumes (one `ServiceCall` per slot).
pub const CALL_SLOTS: u32 = 32;
pub const CALL_SLOT: u32 = 1024;

/// A service call from the dashboard, e.g. ("light", "toggle", "light.kitchen").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceCall {
    pub domain: String,
    pub service: String,
    pub entity_id: String,
    pub data: CallData,
}

/// What a service call carries besides its target.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CallData {
    None,
    /// `light.turn_on`: brightness 0..=255.
    Brightness(u8),
    /// `climate.set_temperature`: the target.
    Temperature(f64),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{decode, encode};

    #[test]
    fn service_calls_fit_a_ring_slot_and_round_trip() {
        for data in [CallData::None, CallData::Brightness(200), CallData::Temperature(20.5)] {
            let c = ServiceCall {
                domain: "climate".into(),
                service: "set_temperature".into(),
                entity_id: "climate.living_room_thermostat_with_a_long_name".into(),
                data,
            };
            let b = encode(&c);
            assert!(b.len() <= CALL_SLOT as usize);
            assert_eq!(decode::<ServiceCall>(&b), Some(c));
        }
    }
}

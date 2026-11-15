//! The Home Assistant WebSocket API as the HA client uses it: the requests
//! it sends (auth, the requests that start a session, service calls,
//! pings) and what the server's answers and events do to the entity store
//! ([`State::apply`]). No I/O: servers/ha owns the connection.
//!
//! A session: the server sends `auth_required` ([`server_version`]), we
//! send [`auth`], it answers ([`auth_result`]); then the [`START`]
//! requests, after which results and `state_changed` events arrive in any
//! mix, each applied to the [`State`].

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use serde_json::{Value, json};
use touchos_proto::ha::{CallData, Entity, ServiceCall, Snapshot};

/// The ids of the requests that start a session ([`START`]); later
/// requests (service calls, pings) count up from [`id::FIRST_FREE`].
pub mod id {
    pub const GET_STATES: u64 = 1;
    pub const SUBSCRIBE: u64 = 2;
    pub const AREAS: u64 = 3;
    pub const DEVICES: u64 = 4;
    pub const ENTITIES: u64 = 5;
    pub const FIRST_FREE: u64 = 6;
}

/// The requests after auth: every state, the change events, and the
/// registries that give the rooms (areas, the devices' areas, and the
/// entity registry's display list). The devices must come before the
/// entities: an entity without its own area takes its device's.
pub const START: [&str; 5] = [
    r#"{"id":1,"type":"get_states"}"#,
    r#"{"id":2,"type":"subscribe_events","event_type":"state_changed"}"#,
    r#"{"id":3,"type":"config/area_registry/list"}"#,
    r#"{"id":4,"type":"config/device_registry/list"}"#,
    r#"{"id":5,"type":"config/entity_registry/list_for_display"}"#,
];

/// The server's first message: its version, if it is `auth_required`.
pub fn server_version(hello: &Value) -> Option<&str> {
    (hello["type"] == "auth_required").then(|| hello["ha_version"].as_str().unwrap_or("?"))
}

pub fn auth(token: &str) -> String {
    json!({"type": "auth", "access_token": token}).to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Auth {
    Ok,
    /// The token was refused.
    Invalid,
    Unexpected,
}

pub fn auth_result(msg: &Value) -> Auth {
    match msg["type"].as_str() {
        Some("auth_ok") => Auth::Ok,
        Some("auth_invalid") => Auth::Invalid,
        _ => Auth::Unexpected,
    }
}

/// A service call from the dashboard as request `id`.
pub fn call_service(id: u64, c: &ServiceCall) -> String {
    let mut m = json!({
        "id": id, "type": "call_service",
        "domain": c.domain, "service": c.service,
        "target": {"entity_id": c.entity_id},
    });
    match c.data {
        CallData::None => {}
        CallData::Brightness(b) => m["service_data"] = json!({ "brightness": b }),
        CallData::Temperature(t) => m["service_data"] = json!({ "temperature": t }),
    }
    m.to_string()
}

/// An application-level ping (the answer is [`Applied::Pong`]).
pub fn ping(id: u64) -> String {
    json!({"id": id, "type": "ping"}).to_string()
}

/// An entity from a state object (`get_states`' list, an event's
/// `new_state`); None without an entity id.
pub fn entity_from_json(s: &Value) -> Option<Entity> {
    let attrs = &s["attributes"];
    let id = s["entity_id"].as_str()?.to_string();
    let dimmable = attrs["supported_color_modes"]
        .as_array()
        .is_some_and(|m| m.iter().any(|v| v.as_str().is_some_and(|c| c != "onoff")));
    Some(Entity {
        name: attrs["friendly_name"].as_str().unwrap_or(&id).to_string(),
        state: s["state"].as_str().unwrap_or("").to_string(),
        unit: attrs["unit_of_measurement"].as_str().unwrap_or("").to_string(),
        brightness: attrs["brightness"].as_u64().map(|b| b.min(255) as u8),
        dimmable,
        current_temp: attrs["current_temperature"].as_f64(),
        target_temp: attrs["temperature"].as_f64(),
        temp_step: attrs["target_temp_step"].as_f64().unwrap_or(0.5),
        position: attrs["current_position"].as_u64().map(|p| p.min(100) as u8),
        id,
    })
}

/// What a message did.
#[derive(Debug, Clone, PartialEq)]
pub enum Applied {
    /// The answer to `get_states`: the store holds every entity now.
    States,
    /// The registries are in: rooms and hidden entities.
    Registry,
    /// An entity changed (or was removed: it is no longer in the store).
    Changed(String),
    /// The answer to a ping.
    Pong(Option<u64>),
    /// A request failed.
    Failed { id: Option<u64>, message: String },
    /// Nothing the store keeps (or not yet: the areas and devices wait for
    /// the entity registry).
    Nothing,
}

/// The entity store: what the dashboard sees, and what it takes to build it.
pub struct State {
    pub view: Snapshot,
    /// Device id -> area id, from the device registry.
    device_area: BTreeMap<String, String>,
}

impl Default for State {
    fn default() -> State {
        State::new()
    }
}

impl State {
    pub const fn new() -> State {
        State {
            view: Snapshot {
                entities: BTreeMap::new(),
                connected: false,
                version: 0,
                areas: Vec::new(),
                entity_area: BTreeMap::new(),
                hidden: BTreeSet::new(),
                registry_version: 0,
            },
            device_area: BTreeMap::new(),
        }
    }

    /// Apply one message from the server.
    pub fn apply(&mut self, msg: &Value) -> Applied {
        let st = &mut self.view;
        let id = msg["id"].as_u64();
        match msg["type"].as_str() {
            Some("result") if msg["success"] == true => {
                let r = &msg["result"];
                match id {
                    Some(id::GET_STATES) => {
                        let Some(list) = r.as_array() else { return Applied::Nothing };
                        st.entities = list.iter().filter_map(entity_from_json).map(|e| (e.id.clone(), e)).collect();
                        st.connected = true;
                        st.version += 1;
                        Applied::States
                    }
                    Some(id::AREAS) => {
                        st.areas = pairs(r, "area_id", "name").collect();
                        st.areas.sort_by(|a, b| a.1.cmp(&b.1));
                        Applied::Nothing
                    }
                    Some(id::DEVICES) => {
                        self.device_area = pairs(r, "id", "area_id").collect();
                        Applied::Nothing
                    }
                    Some(id::ENTITIES) => {
                        // Compact format: ei = entity id, di = device, ai = area,
                        // ec = category, hb = hidden.
                        let (mut ea, mut hidden) = (BTreeMap::new(), BTreeSet::new());
                        for e in r["entities"].as_array().map(|v| v.as_slice()).unwrap_or(&[]) {
                            let Some(eid) = e["ei"].as_str() else { continue };
                            let area = e["ai"]
                                .as_str()
                                .or_else(|| e["di"].as_str().and_then(|d| self.device_area.get(d).map(|s| s.as_str())));
                            if let Some(a) = area {
                                ea.insert(eid.to_string(), a.to_string());
                            }
                            if e["hb"].as_bool() == Some(true) || !e["ec"].is_null() {
                                hidden.insert(eid.to_string());
                            }
                        }
                        st.entity_area = ea;
                        st.hidden = hidden;
                        st.registry_version += 1;
                        st.version += 1;
                        Applied::Registry
                    }
                    _ => Applied::Nothing,
                }
            }
            Some("result") if msg["success"] == false => Applied::Failed {
                id,
                message: msg["error"]["message"].as_str().unwrap_or("?").to_string(),
            },
            Some("event") => {
                let data = &msg["event"]["data"];
                let Some(eid) = data["entity_id"].as_str() else { return Applied::Nothing };
                match entity_from_json(&data["new_state"]) {
                    Some(e) => {
                        st.entities.insert(e.id.clone(), e);
                    }
                    None => {
                        st.entities.remove(eid);
                    }
                }
                st.version += 1;
                Applied::Changed(eid.to_string())
            }
            Some("pong") => Applied::Pong(id),
            _ => Applied::Nothing,
        }
    }
}

/// (a, b) from each object of a registry list that has both as strings.
fn pairs<'v>(list: &'v Value, a: &'v str, b: &'v str) -> impl Iterator<Item = (String, String)> + 'v {
    list.as_array()
        .map(|l| l.as_slice())
        .unwrap_or(&[])
        .iter()
        .filter_map(move |x| Some((x[a].as_str()?.to_string(), x[b].as_str()?.to_string())))
}

#[cfg(test)]
mod tests;

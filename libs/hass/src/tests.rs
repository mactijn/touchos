//! Messages in the shapes Home Assistant sends them (trimmed to the
//! fields that matter, names made up).

use super::*;
use serde_json::from_str;

fn v(s: &str) -> Value {
    from_str(s).unwrap()
}

const STATES: &str = r#"{"id":1,"type":"result","success":true,"result":[
  {"entity_id":"light.kitchen","state":"on","attributes":{"friendly_name":"Kitchen",
    "supported_color_modes":["brightness"],"brightness":180,"color_mode":"brightness"},
   "last_changed":"2026-11-14T20:01:02.123456+00:00","context":{"id":"01J"}},
  {"entity_id":"switch.fan","state":"off","attributes":{"friendly_name":"Fan",
    "supported_color_modes":["onoff"]}},
  {"entity_id":"climate.living","state":"heat","attributes":{"friendly_name":"Living room",
    "current_temperature":19.5,"temperature":21,"target_temp_step":0.5}},
  {"entity_id":"cover.blind","state":"open","attributes":{"current_position":140}},
  {"entity_id":"sensor.power","state":"412","attributes":{"unit_of_measurement":"W",
    "friendly_name":"Power"}},
  {"state":"on","attributes":{}}
]}"#;

#[test]
fn a_session_start() {
    let hello = v(r#"{"type":"auth_required","ha_version":"2026.11.0"}"#);
    assert_eq!(server_version(&hello), Some("2026.11.0"));
    assert_eq!(server_version(&v(r#"{"type":"auth_ok"}"#)), None);
    assert_eq!(v(&auth("abc\"def")), json!({"type":"auth","access_token":"abc\"def"}));
    assert_eq!(auth_result(&v(r#"{"type":"auth_ok","ha_version":"2026.11.0"}"#)), Auth::Ok);
    assert_eq!(auth_result(&v(r#"{"type":"auth_invalid","message":"Invalid access token"}"#)), Auth::Invalid);
    assert_eq!(auth_result(&v(r#"{"type":"result"}"#)), Auth::Unexpected);
    // The start requests carry the ids the answers are matched by.
    let ids: Vec<u64> = START.iter().map(|r| v(r)["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, [id::GET_STATES, id::SUBSCRIBE, id::AREAS, id::DEVICES, id::ENTITIES]);
    assert!(id::FIRST_FREE > *ids.iter().max().unwrap());
}

#[test]
fn states() {
    let mut s = State::new();
    assert_eq!(s.apply(&v(STATES)), Applied::States);
    let e = &s.view.entities;
    assert_eq!(e.len(), 5); // the one without an entity id is skipped
    let k = &e["light.kitchen"];
    assert_eq!((k.name.as_str(), k.state.as_str(), k.brightness, k.dimmable), ("Kitchen", "on", Some(180), true));
    assert!(!e["switch.fan"].dimmable);
    let c = &e["climate.living"];
    assert_eq!((c.current_temp, c.target_temp, c.temp_step), (Some(19.5), Some(21.0), 0.5));
    assert_eq!(e["cover.blind"].position, Some(100)); // clamped
    assert_eq!(e["cover.blind"].name, "cover.blind"); // no friendly name: the id
    assert_eq!(e["sensor.power"].unit, "W");
    assert!(s.view.connected);
    assert_eq!(s.view.version, 1);

    // A second answer (after a reconnect) replaces the store.
    let again = v(r#"{"id":1,"type":"result","success":true,"result":[{"entity_id":"light.hall","state":"off"}]}"#);
    assert_eq!(s.apply(&again), Applied::States);
    assert_eq!(s.view.entities.keys().collect::<Vec<_>>(), ["light.hall"]);
    assert_eq!(s.view.version, 2);
}

#[test]
fn events() {
    let mut s = State::new();
    s.apply(&v(STATES));
    let changed = v(r#"{"id":2,"type":"event","event":{"event_type":"state_changed","data":{
        "entity_id":"light.kitchen",
        "old_state":{"entity_id":"light.kitchen","state":"on","attributes":{}},
        "new_state":{"entity_id":"light.kitchen","state":"off","attributes":{"friendly_name":"Kitchen",
          "supported_color_modes":["brightness"],"brightness":null}}},
        "origin":"LOCAL","time_fired":"2026-11-14T20:05:00+00:00"}}"#);
    assert_eq!(s.apply(&changed), Applied::Changed("light.kitchen".into()));
    let k = &s.view.entities["light.kitchen"];
    assert_eq!((k.state.as_str(), k.brightness), ("off", None));
    assert_eq!(s.view.version, 2);

    let removed = v(r#"{"id":2,"type":"event","event":{"event_type":"state_changed","data":{
        "entity_id":"switch.fan","old_state":{"entity_id":"switch.fan","state":"off"},"new_state":null}}}"#);
    assert_eq!(s.apply(&removed), Applied::Changed("switch.fan".into()));
    assert!(!s.view.entities.contains_key("switch.fan"));

    let added = v(r#"{"id":2,"type":"event","event":{"data":{"entity_id":"light.new",
        "old_state":null,"new_state":{"entity_id":"light.new","state":"on","attributes":{}}}}}"#);
    s.apply(&added);
    assert_eq!(s.view.entities["light.new"].state, "on");

    // No entity id: nothing changes.
    assert_eq!(s.apply(&v(r#"{"id":2,"type":"event","event":{"data":{}}}"#)), Applied::Nothing);
    assert_eq!(s.view.version, 4);
}

#[test]
fn rooms() {
    let mut s = State::new();
    let areas = v(r#"{"id":3,"type":"result","success":true,"result":[
        {"area_id":"living_room","name":"Living room","aliases":[],"floor_id":null},
        {"area_id":"kitchen","name":"Kitchen","picture":null},
        {"area_id":"broken"}]}"#);
    assert_eq!(s.apply(&areas), Applied::Nothing);
    assert_eq!(
        s.view.areas,
        [("kitchen".into(), "Kitchen".into()), ("living_room".into(), "Living room".into())]
    );
    let devices = v(r#"{"id":4,"type":"result","success":true,"result":[
        {"id":"dev1","area_id":"kitchen","name":"Hue bridge"},
        {"id":"dev2","area_id":null,"name":"Plug"}]}"#);
    assert_eq!(s.apply(&devices), Applied::Nothing);
    let entities = v(r#"{"id":5,"type":"result","success":true,"result":{
        "entity_categories":{"0":"config","1":"diagnostic"},
        "entities":[
          {"ei":"light.kitchen","di":"dev1","pl":"hue"},
          {"ei":"climate.living","ai":"living_room"},
          {"ei":"light.spot","di":"dev1","ai":"living_room"},
          {"ei":"switch.plug","di":"dev2"},
          {"ei":"sensor.rssi","di":"dev1","ec":1},
          {"ei":"light.old","hb":true},
          {"di":"dev1"}]}}"#);
    assert_eq!(s.apply(&entities), Applied::Registry);
    let ea = &s.view.entity_area;
    assert_eq!(ea["light.kitchen"], "kitchen"); // the device's area
    assert_eq!(ea["climate.living"], "living_room");
    assert_eq!(ea["light.spot"], "living_room"); // its own area wins
    assert!(!ea.contains_key("switch.plug"));
    assert_eq!(s.view.hidden.iter().collect::<Vec<_>>(), ["light.old", "sensor.rssi"]);
    assert_eq!((s.view.registry_version, s.view.version), (1, 1));
}

#[test]
fn answers_that_are_not_state() {
    let mut s = State::new();
    let failed = v(r#"{"id":7,"type":"result","success":false,"error":{"code":"not_found","message":"Service light.nope not found."}}"#);
    assert_eq!(s.apply(&failed), Applied::Failed { id: Some(7), message: "Service light.nope not found.".into() });
    assert_eq!(s.apply(&v(r#"{"id":9,"type":"pong"}"#)), Applied::Pong(Some(9)));
    assert_eq!(s.apply(&v(r#"{"id":8,"type":"result","success":true,"result":null}"#)), Applied::Nothing);
    // get_states answered with something other than a list.
    assert_eq!(s.apply(&v(r#"{"id":1,"type":"result","success":true,"result":{}}"#)), Applied::Nothing);
    assert_eq!(s.apply(&v(r#"{"type":"something_new"}"#)), Applied::Nothing);
    assert_eq!(s.apply(&v("[1,2]")), Applied::Nothing);
    assert!(!s.view.connected);
    assert_eq!(s.view.version, 0);
}

#[test]
fn requests() {
    let call = |data| ServiceCall { domain: "light".into(), service: "turn_on".into(), entity_id: "light.kitchen".into(), data };
    assert_eq!(
        v(&call_service(6, &call(CallData::None))),
        json!({"id":6,"type":"call_service","domain":"light","service":"turn_on","target":{"entity_id":"light.kitchen"}})
    );
    assert_eq!(v(&call_service(7, &call(CallData::Brightness(128))))["service_data"], json!({"brightness":128}));
    assert_eq!(v(&call_service(8, &call(CallData::Temperature(20.5))))["service_data"], json!({"temperature":20.5}));
    assert_eq!(v(&ping(12)), json!({"id":12,"type":"ping"}));
}

//! Events given to one handler of an exported world, and what the Python rule did with each (Issue
//! #225, lane M1; `tests/support/core_texts.py` writes the files).
//!
//! A world of such a file is an exported world, or a variant of another world of the same file
//! (`{"base": name, "changes": [...]}`): `entry` (`resolve` of a file read already) makes the
//! entry again, applying each change in order (`["set", path, value]`, where an index one past a
//! list's end appends, and `["del", path]`). A case is one event for one handler of one world: the
//! outcome the Python rule raised (`[class, text]`), or `after`, the world it left when the event
//! applied. `check` runs the core's handler (`dispatch::apply_alone`, the kernel the handler's
//! PyO3 entry calls) on a fresh load of the world with its host (`load::loaded`, and `host`:
//! `load::host` whose `find_record` finds the world's records) and checks that it raised that
//! outcome itself, with the same text, and changed nothing (or, where the case has `after` too, left
//! that world, as a rule that writes before its outcome does); or that it applied and left the world
//! `after` is, byte for byte (`canonical::parts`).

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::events::{EventRow, EventStatus};
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::rules::dispatch;
use sheltersim_core::rules::tick::Tick;
use sheltersim_core::run::dispatch::{Handler, HandlerKind, Native};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::world::WorldCell;

use super::canonical::parts;
use super::load::{self, Loaded};
use super::TestHost;

// ---- Worlds -------------------------------------------------------------------------------------

/// The named world of a fixture file of cases, its variants made again.
pub fn entry(file: &str, world: &str) -> Value {
    resolve(&load::data(file)["worlds"], world)
}

/// The named world of a file read already (`load::data`).
pub fn resolve(worlds: &Value, name: &str) -> Value {
    let world = worlds.get(name).unwrap_or_else(|| panic!("no world {name}"));
    let Some(base) = world.get("base") else {
        return world.clone();
    };
    let mut entry = resolve(worlds, base.as_str().unwrap());
    for change in world["changes"].as_array().unwrap() {
        apply(&mut entry, change);
    }
    entry
}

fn apply(entry: &mut Value, change: &Value) {
    let path = change[1].as_array().unwrap();
    let (last, parents) = path.split_last().expect("a change below the entry");
    let mut node = entry;
    for key in parents {
        node = match key {
            Value::String(key) => node.get_mut(key.as_str()),
            index => node.get_mut(index.as_u64().unwrap() as usize),
        }
        .unwrap_or_else(|| panic!("no {key} on the path {path:?}"));
    }
    match (change[0].as_str().unwrap(), last) {
        ("set", Value::String(key)) => {
            node.as_object_mut().unwrap().insert(key.clone(), change[2].clone());
        }
        ("set", index) => {
            let items = node.as_array_mut().unwrap();
            let index = index.as_u64().unwrap() as usize;
            if index == items.len() {
                items.push(change[2].clone());
            } else {
                items[index] = change[2].clone();
            }
        }
        ("del", Value::String(key)) => {
            node.as_object_mut().unwrap().remove(key.as_str()).unwrap();
        }
        (op, _) => panic!("not a change: {op} {path:?}"),
    }
}

// ---- Handlers -----------------------------------------------------------------------------------

/// The handler of a case: a class with the values the Run reads of it, or the controller's handler
/// of an event type, with some of those values replaced.
fn native(loaded: &Loaded, handler: &Value) -> Native {
    let mut native = match (handler.get("class"), handler.get("table")) {
        (Some(class), _) => {
            let (module, qualname) = (class[0].as_str().unwrap(), class[1].as_str().unwrap());
            let kind = HandlerKind::of_class(module, qualname).unwrap_or_else(|| panic!("the core does not run {qualname}"));
            let object = Obj::new(Class { module: Some(Arc::from(module)), qualname: Some(Arc::from(qualname)), resolves: true });
            Native { kind, handler: Arc::new(object), own_managers: true, after_arrival_as_defined: true }
        }
        (None, Some(event_type)) => match loaded.policies.dispatch.handler(event_type.as_str().unwrap()).unwrap() {
            Some(Handler::Native(native)) => native.clone(),
            _ => panic!("the controller has no handler of {event_type} the core runs"),
        },
        _ => panic!("not a handler: {handler}"),
    };
    let object = Arc::make_mut(&mut native.handler);
    for (name, value) in handler["attrs"].as_object().unwrap() {
        object.attrs.insert(Arc::from(name.as_str()), Ok(Held::Value(load::v(value))));
    }
    native
}

// ---- Cases --------------------------------------------------------------------------------------

/// `load::host` of the entry, whose `find_record` finds the world's records as the native host's
/// does (`event_log.find`).
pub fn host(entry: &Value) -> TestHost {
    let mut host = load::host(entry);
    for record in entry["world"]["event_log"].as_array().unwrap() {
        let event = load::event(&record["event"]);
        let status = EventStatus::parse(record["status"].as_str().unwrap()).unwrap();
        let logged = Logged { status, realized_minute: record["realized_minute"].as_i64().unwrap(), event };
        host.records.entry(logged.event.event_id.clone()).or_insert(logged);
    }
    host
}

/// What a handler (`handler`, as a case holds it) gives for an event on a loaded world with its
/// host: `dispatch::apply_alone`, the kernel the handler's PyO3 entry calls, at the world's minute.
pub fn apply_handler(loaded: &Loaded, host: &mut TestHost, handler: &Value, event: &EventRow) -> Result<Option<(EventStatus, String)>, String> {
    let native = native(loaded, handler);
    let minute = loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
    let mut tick = Tick {
        cell: &loaded.cell,
        host,
        run: &loaded.run,
        policies: loaded.policies.clone(),
        minute,
        seed: loaded.seed.clone(),
        neighbors: loaded.neighbors.clone(),
    };
    dispatch::apply_alone(&mut tick, &native, event).map_err(|error| format!("{error:?}"))
}

/// What the handler of a case gives on a fresh load of its world, and the world's parts before and
/// after.
fn run(data: &Value, case: &Value) -> (Result<Option<(EventStatus, String)>, String>, Vec<(&'static str, Vec<u8>)>, Loaded) {
    let entry = resolve(&data["worlds"], case["world"].as_str().unwrap());
    let loaded = load::loaded(&entry);
    let mut host = host(&entry);
    let event: EventRow = load::event(&case["event"]);
    let before = loaded.cell.with(|world| parts(world)).unwrap();
    let result = apply_handler(&loaded, &mut host, &case["handler"], &event);
    (result, before, loaded)
}

/// The names of the parts that differ.
fn differing(first: &[(&'static str, Vec<u8>)], second: &[(&'static str, Vec<u8>)]) -> Vec<&'static str> {
    first.iter().zip(second).filter(|(one, other)| one != other).map(|(one, _)| one.0).collect()
}

/// Check one case of a file read already (`load::data`).
pub fn check(data: &Value, case: &Value) {
    let (result, before, loaded) = run(data, case);
    let now = loaded.cell.with(|world| parts(world)).unwrap();
    let event = &case["event"];
    if let Some(outcome) = case.get("outcome") {
        let status = match outcome[0].as_str().unwrap() {
            "Suppressed" => EventStatus::Suppressed,
            "Failed" => EventStatus::Failed,
            other => panic!("not an outcome: {other}"),
        };
        let wanted = Ok(Some((status, outcome[1].as_str().unwrap().to_owned())));
        assert_eq!(result, wanted, "{event}");
        match case.get("after") {
            // A rule that wrote before its outcome (a stop, then the event suppressed).
            Some(after) => {
                let wanted = load::loaded(&resolve(&data["worlds"], after.as_str().unwrap())).cell.with(|world| parts(world)).unwrap();
                assert!(now != before, "the handler changed nothing: {event}");
                assert!(now == wanted, "the handler left {:?} otherwise: {event}", differing(&wanted, &now));
            }
            None => assert!(now == before, "the handler changed {:?}: {event}", differing(&before, &now)),
        }
    } else {
        assert_eq!(result, Ok(None), "{event}");
        let after = resolve(&data["worlds"], case["after"].as_str().unwrap());
        let wanted = load::loaded(&after).cell.with(|world| parts(world)).unwrap();
        assert!(now == wanted, "the handler left {:?} otherwise: {event}", differing(&wanted, &now));
    }
}

/// Check every case of a group, which has `count` of them.
pub fn check_group(file: &str, group: &str, count: usize) {
    let data = load::data(file);
    let cases = data["cases"].get(group).and_then(Value::as_array).unwrap_or_else(|| panic!("{file} has no group {group}"));
    assert_eq!(cases.len(), count, "{group}");
    for case in cases {
        check(&data, case);
    }
}

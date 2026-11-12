//! The typed rows of `model_state` read from JSON without Python (Issue #225, F1; #214).
//!
//! `tests/data/typed_rows.json` holds worlds of `tests/support/core_typed_rows.py` (written by
//! `benchmarks/core_fixtures.py`, checked by `tests/integration/test_core_fixtures.py`)
//! that together have every typed namespace: queues and their stalls, queue and staff support
//! trips, a working and a finished staff task, a shift config with and without its optional keys,
//! histories, receipts and audit records. Each namespace read into its row with `Field::from_value`
//! writes the bytes of its canonical JSON again, and the loader puts it in its slot as that row.
//!
//! What the PyO3 crate's `PyField::from_py` refuses is refused here too: the other number type, a
//! missing or an extra key, a text outside an enum's set, a null where no `Option` is, and the
//! checks of the hand-written readers (a journey's optional keys, the off-shift recovery's ints,
//! the audit's records, the depth of a value).

mod support;

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::json::Encoder;
use sheltersim_core::row::Row;
use sheltersim_core::value::{MAX_DEPTH, V};
use support::load;
use support::typed::{typed_row, TYPED};

fn fixture() -> Value {
    let raw = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/typed_rows.json")).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn bytes(row: &dyn Row) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(1 << 16);
    row.write_json(&mut encoder).unwrap();
    encoder.out
}

fn canonical(value: &V) -> Vec<u8> {
    let mut encoder = Encoder::with_capacity(1 << 16);
    value.write_json(&mut encoder).unwrap();
    encoder.out
}

/// A namespace of a world of the fixture, as the core's `V`.
fn namespace(world: usize, name: &str) -> V {
    load::v(&fixture()["worlds"][world]["model_state"][name])
}

fn get<'a>(value: &'a mut V, path: &[&str]) -> &'a mut V {
    path.iter().fold(value, |value, key| match value {
        V::Map(map) => map.get_mut(*key).unwrap_or_else(|| panic!("no {key}")),
        V::List(items) => &mut items[key.parse::<usize>().unwrap()],
        other => panic!("{key} of {other:?}"),
    })
}

fn map(value: &mut V) -> &mut indexmap::IndexMap<std::sync::Arc<str>, V> {
    match value {
        V::Map(map) => map,
        other => panic!("not a map: {other:?}"),
    }
}

/// The first key of a map at path.
fn first_key(value: &mut V, path: &[&str]) -> String {
    map(get(value, path)).keys().next().unwrap().to_string()
}

/// The reason the row refuses this namespace of the first world after change.
fn refused(name: &str, change: impl FnOnce(&mut V)) -> String {
    refused_in(0, name, change)
}

fn refused_in(world: usize, name: &str, change: impl FnOnce(&mut V)) -> String {
    let mut value = namespace(world, name);
    change(&mut value);
    match typed_row(name, &value).unwrap() {
        Ok(_) => panic!("{name} took the changed value"),
        Err(misfit) => misfit.0,
    }
}

/// The bytes the row writes of this namespace of the first world after change, which it takes.
fn taken(name: &str, change: impl FnOnce(&mut V)) -> (Vec<u8>, Vec<u8>) {
    let mut value = namespace(0, name);
    change(&mut value);
    let row = typed_row(name, &value).unwrap().unwrap_or_else(|misfit| panic!("{name}: {}", misfit.0));
    (bytes(&*row), canonical(&value))
}

#[test]
fn the_table_has_every_typed_namespace_python_has() {
    let fixture = fixture();
    let python: Vec<&str> = fixture["typed"].as_array().unwrap().iter().map(|name| name.as_str().unwrap()).collect();
    let table: Vec<&str> = TYPED.iter().map(|(name, _)| *name).collect();
    assert_eq!(table, python);
    assert!(typed_row("population_presence_v1", &V::Null).is_none());
}

#[test]
fn every_namespace_reads_into_its_row_and_writes_the_same_bytes() {
    let fixture = fixture();
    let mut seen = std::collections::BTreeSet::new();
    for world in fixture["worlds"].as_array().unwrap() {
        for (name, value) in world["model_state"].as_object().unwrap() {
            let value = load::v(value);
            let row = typed_row(name, &value).unwrap().unwrap_or_else(|misfit| panic!("{name}: {}", misfit.0));
            let written = bytes(&*row);
            assert!(written == canonical(&value), "{name} at minute {} wrote other bytes", world["minute"]);
            // The bytes Python's json writes of the namespace.
            assert_eq!(sha256_hex(&written), world["digests"][name].as_str().unwrap(), "{name}");
            seen.insert(name.clone());
        }
    }
    assert_eq!(seen.len(), TYPED.len());
}

#[test]
fn the_loader_puts_each_typed_namespace_in_its_slot_as_its_row() {
    // The tick test's world, whose controller owns every namespace, with the model_state of a
    // world that has them all.
    let tick = load::fixture();
    let mut document = tick["world"].clone();
    document["model_state"] = fixture()["worlds"][0]["model_state"].clone();
    let world = load::world(&document, &tick["slots"]);
    for (name, value) in document["model_state"].as_object().unwrap() {
        let value = load::v(value);
        let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
        let held = slot.row.as_deref().unwrap();
        let read = typed_row(name, &value).unwrap().unwrap();
        assert_eq!(held.as_any().type_id(), read.as_any().type_id(), "{name}");
        assert!(bytes(held) == canonical(&value), "{name}");
    }
}

#[test]
fn a_number_of_the_other_type_is_refused() {
    let agent = |value: &mut V| first_key(value, &["agents"]);
    assert_eq!(
        refused("body_state_v1", |value| {
            let id = agent(value);
            *get(value, &["agents", &id, "sleep_debt_minutes"]) = V::Int(1);
        }),
        "not a float"
    );
    assert_eq!(refused("movement_v1", |value| *get(value, &["last_updated_minute"]) = V::Float(10.0)), "not an int");
    assert_eq!(refused("social_network_v1", |value| *get(value, &["interaction_count"]) = V::Bool(true)), "not an int");
    assert_eq!(refused("social_network_v1", |value| *get(value, &["interaction_count"]) = V::Big("9223372036854775808".into())), "an int too large");
    assert_eq!(refused("shared_operations_v1", |value| *get(value, &["history_truncated"]) = V::Int(0)), "not a bool");
    assert_eq!(refused("resource_ledger_v1", |value| *get(value, &["warehouse_id"]) = V::Int(1)), "not a str");
}

#[test]
fn a_record_takes_its_keys_and_no_others() {
    assert_eq!(
        refused("movement_v1", |value| {
            map(value).insert("extra".into(), V::Null);
        }),
        "other fields than the row's"
    );
    assert_eq!(
        refused("movement_v1", |value| {
            map(value).shift_remove("schema_version");
        }),
        "other fields than the row's: no schema_version"
    );
    // A missing key and an extra one: as many keys as the row has, but not its keys.
    assert_eq!(
        refused("movement_v1", |value| {
            let record = map(value);
            record.shift_remove("schema_version");
            record.insert("extra".into(), V::Int(1));
        }),
        "other fields than the row's: no schema_version"
    );
    assert_eq!(
        refused("body_state_v1", |value| {
            let id = first_key(value, &["agents"]);
            map(get(value, &["agents", &id])).insert("extra".into(), V::Float(0.0));
        }),
        "other fields than the row's"
    );
    assert_eq!(refused("body_state_v1", |value| *get(value, &["agents"]) = V::List(Vec::new())), "not a dict");
    assert_eq!(refused("resource_ledger_v1", |value| *get(value, &["used_reservation_ids"]) = V::Map(Default::default())), "not a list");
}

#[test]
fn a_key_a_record_may_lack_is_absent_from_its_bytes() {
    // The staff dynamics config of the first world has its per-staff limits; the third's has none.
    let (written, canonical) = taken("staff_dynamics_v1", |value| {
        map(get(value, &["config"])).shift_remove("max_continuous_work_minutes_by_staff").unwrap();
        map(get(value, &["config"])).shift_remove("off_shift_fatigue_recovery_per_minute").unwrap();
    });
    assert!(written == canonical);
    assert!(!String::from_utf8(written).unwrap().contains("max_continuous_work_minutes_by_staff"));
    let mut operations = namespace(2, "staff_dynamics_v1");
    assert!(!map(get(&mut operations, &["config"])).contains_key("max_continuous_work_minutes_by_staff"));
}

#[test]
fn an_enum_takes_the_texts_of_its_set() {
    let staff = |value: &mut V| first_key(value, &["staff"]);
    assert_eq!(
        refused("staff_operations_v1", |value| {
            let id = staff(value);
            *get(value, &["staff", &id, "phase"]) = V::Str("sleeping".into());
        }),
        "not a Phase"
    );
    assert_eq!(
        refused("staff_operations_v1", |value| {
            let id = staff(value);
            *get(value, &["staff", &id, "phase"]) = V::Int(0);
        }),
        "not a str"
    );
    assert_eq!(
        refused("staff_dynamics_v1", |value| {
            let id = first_key(value, &["staff"]);
            *get(value, &["staff", &id, "last_activity"]) = V::Str("Working".into());
        }),
        "not a LastActivity"
    );
}

#[test]
fn a_null_fits_an_option_only() {
    let (written, canonical) = taken("resource_ledger_v1", |value| *get(value, &["zone_id"]) = V::Null);
    assert!(written == canonical);
    assert_eq!(refused("resource_ledger_v1", |value| *get(value, &["warehouse_id"]) = V::Null), "not a str");
}

#[test]
fn a_journey_has_its_ten_keys_and_the_optional_three_only_with_a_text() {
    // The first world's queue trip.
    let queue_trip = |value: &mut V| {
        let journeys = map(get(value, &["journeys"]));
        let id = journeys.iter().find(|(_, journey)| matches!(journey, V::Map(record) if record.contains_key("queue_id"))).unwrap().0;
        id.to_string()
    };
    assert_eq!(
        refused("movement_v1", |value| {
            let id = queue_trip(value);
            *get(value, &["journeys", &id, "queue_id"]) = V::Null;
        }),
        "not a str"
    );
    assert_eq!(
        refused("movement_v1", |value| {
            let id = queue_trip(value);
            map(get(value, &["journeys", &id])).shift_remove("route_points");
        }),
        "a journey field is missing"
    );
    assert_eq!(
        refused("movement_v1", |value| {
            let id = queue_trip(value);
            map(get(value, &["journeys", &id])).insert("extra".into(), V::Int(1));
        }),
        "other fields than the row's"
    );
    let (written, canonical) = taken("movement_v1", |value| {
        let id = queue_trip(value);
        map(get(value, &["journeys", &id])).shift_remove("queue_id");
    });
    assert!(written == canonical);
}

#[test]
fn the_off_shift_recovery_is_a_float_or_an_int_a_float_holds() {
    let recovery = |number: V| move |value: &mut V| *get(value, &["config", "off_shift_fatigue_recovery_per_minute"]) = number;
    assert_eq!(refused("staff_dynamics_v1", recovery(V::Int((1 << 53) + 1))), "an int a float can't hold");
    assert_eq!(refused("staff_dynamics_v1", recovery(V::Bool(false))), "not a float or an int");
    let (written, canonical) = taken("staff_dynamics_v1", recovery(V::Int(1)));
    assert!(written == canonical);
    let (written, canonical) = taken("staff_dynamics_v1", recovery(V::Float(1.0)));
    assert!(written == canonical);
}

#[test]
fn the_audit_takes_the_records_the_rules_take_of_one_warehouse() {
    assert_eq!(refused("resource_ledger_v1", |value| *get(value, &["audit", "0", "quantity"]) = V::Int(-1)), "an audit record the rules turn down");
    assert_eq!(refused("resource_ledger_v1", |value| *get(value, &["audit", "1", "warehouse_id"]) = V::Str("other".into())), "audit records of two warehouses");
}

#[test]
fn a_value_nests_as_deep_as_the_pyo3_readers_take() {
    let nested = |depth: usize| (0..depth).fold(V::Null, |inner, _| V::List(vec![inner]));
    // A free-form field: the config of the staff operations.
    let (written, canonical) = taken("staff_operations_v1", |value| *get(value, &["config"]) = nested(MAX_DEPTH));
    assert!(written == canonical);
    assert_eq!(refused("staff_operations_v1", |value| *get(value, &["config"]) = nested(MAX_DEPTH + 1)), "value nests too deeply");
    // A history item, kept as its bytes.
    assert_eq!(
        refused("body_state_v1", |value| match get(value, &["history"]) {
            V::List(items) => items.push(nested(MAX_DEPTH + 1)),
            other => panic!("{other:?}"),
        }),
        "value nests too deeply"
    );
}

//! The operations of the resource ledger (`rules::resources::apply_operation`, the kernel of
//! `apply_resource_operation`) on the world of `integration/test_resource_ledger_row.py`, without
//! Python (Issue #225, lane M10b).
//!
//! `data/resource_ledger.json` (`tests/support/core_fixtures_reads.py`) holds that world: the small
//! replay Scenario with a stock of water and meals and no events, at minute 0, and the operations
//! the pytest file drew with `random.Random`. What an operation wrote is pinned as the pytest file
//! pinned it, with the same digests, taken from the deleted Python rule: `json.dumps` with
//! `ensure_ascii=False` of the ledger's tree and of `world.resources` (`Dumps`). The tree is the
//! row's, each record's fields in the order of its row (`NAMES`), the audit records' keys sorted,
//! as the tree that follows the row has them. A refusal has the Python rule's text and changes
//! nothing (`canonical_of`). The entry writes the new balance into `world.resources`; here
//! `operate` does, as the PyO3 crate's `replace_resources` does.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::error::Error;
use sheltersim_core::json::Encoder;
use sheltersim_core::row::IdMap;
use sheltersim_core::rules::resources::{self, Applied, Fail, OperationArg, OperationEvent};
use sheltersim_core::rules::service::repr_of;
use sheltersim_core::state::resource_ledger::{Account, Accounts, LedgerRow, Reservation, NAME, RESOURCE_IDS};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell, RESOURCES};

use support::dumps::Dumps;
use support::{load, texts};

const FILE: &str = "resource_ledger";
const SOURCE: &str = "ASM-TEST-RESOURCE-OPERATION";

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// The world of the pytest file (`_world`), in a cell, with its entry.
fn ledger() -> (Value, Shard<World>) {
    let entry = load::entry(FILE, "ledger");
    let cell = texts::cell(&entry);
    (entry, cell)
}

fn set_minute(cell: &Shard<World>, minute: i64) {
    cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute)).unwrap();
}

/// An event's payload: the warehouse, then the given fields in order (`_event`).
fn payload(fields: &[(&str, V)]) -> V {
    let mut map = IndexMap::new();
    map.insert(Arc::from("warehouse_id"), text("warehouse-main"));
    for (name, value) in fields {
        map.insert(Arc::from(*name), value.clone());
    }
    V::Map(map)
}

/// An event's payload as given, without the warehouse.
fn exactly(fields: &[(&str, V)]) -> V {
    V::Map(fields.iter().map(|(name, value)| (Arc::from(*name), value.clone())).collect())
}

/// `apply_resource_operation` of an event with this payload, source and ID, and the new balance in
/// `world.resources` (a new dict in Python, here the row).
fn operate(cell: &Shard<World>, operation: OperationArg<'_>, payload: &V, source: &V, event_id: &str) -> Result<Applied, Fail> {
    let event_id = text(event_id);
    let event = OperationEvent { payload, source_id: source, event_id: &event_id };
    cell.with(|world| {
        let applied = resources::apply_operation(world, &event, operation)?;
        world.row_mut::<IdMap<i64>>(RESOURCES).unwrap().insert(Arc::from(RESOURCE_IDS[applied.index]), applied.remaining);
        Ok(applied)
    })
    .unwrap()
}

/// The pytest file's `_apply`: the operation applies.
fn apply(cell: &Shard<World>, operation: &str, fields: &[(&str, V)], event_id: &str) {
    operate(cell, OperationArg::Text(operation), &payload(fields), &text(SOURCE), event_id).unwrap_or_else(|fail| panic!("{event_id}: {fail:?}"));
}

/// The text of a `ResourceLedgerError`, with Python's `repr` of a payload value.
fn ledger_text(result: Result<Applied, Fail>) -> String {
    match result {
        Err(Fail::Ledger(fault)) => fault.text(repr_of),
        other => panic!("not a ResourceLedgerError: {other:?}"),
    }
}

fn first_agent(cell: &Shard<World>) -> String {
    cell.with(|world| {
        let rows = world.rows().unwrap();
        let mut ids: Vec<&str> = rows.agents.iter().map(|(id, _)| &**id).collect();
        ids.sort_unstable();
        ids[0].to_owned()
    })
    .unwrap()
}

fn sorted_agents(cell: &Shard<World>) -> Vec<String> {
    cell.with(|world| {
        let rows = world.rows().unwrap();
        let mut ids: Vec<String> = rows.agents.iter().map(|(id, _)| id.to_string()).collect();
        ids.sort_unstable();
        ids
    })
    .unwrap()
}

fn meal(agent: &str, reservation: &str, quantity: Option<i64>) -> Vec<(&'static str, V)> {
    let mut fields = vec![("resource_id", text("food_meal_units")), ("agent_id", text(agent)), ("reservation_id", text(reservation))];
    if let Some(quantity) = quantity {
        fields.insert(1, ("quantity", V::Int(quantity)));
    }
    fields
}

/// The pytest file's `_operate`: a delivery, a reservation of two meals, one consumed, another
/// reserved and released, and a loss of water; the events `op-1` to `op-6`. The person it gave.
fn operate_all(cell: &Shard<World>) -> String {
    let agent = first_agent(cell);
    set_minute(cell, 1);
    apply(cell, "delivery", &[("resource_id", text("water_ml")), ("quantity", V::Int(500))], "op-1");
    set_minute(cell, 2);
    apply(cell, "reserve", &meal(&agent, "r-1", Some(2)), "op-2");
    set_minute(cell, 3);
    apply(cell, "consume", &meal(&agent, "r-1", Some(1)), "op-3");
    set_minute(cell, 4);
    apply(cell, "reserve", &meal(&agent, "r-2", Some(1)), "op-4");
    apply(cell, "release", &meal(&agent, "r-2", None), "op-5");
    set_minute(cell, 9);
    apply(cell, "loss", &[("resource_id", text("water_ml")), ("quantity", V::Int(200))], "op-6");
    agent
}

/// A map with its keys in the order of these names.
fn in_order(value: V, names: &[&str]) -> V {
    let V::Map(mut map) = value else { panic!("not a map") };
    let mut out = IndexMap::with_capacity(map.len());
    for name in names {
        let (key, item) = map.shift_remove_entry(*name).unwrap_or_else(|| panic!("no {name}"));
        out.insert(key, item);
    }
    assert!(map.is_empty(), "{map:?}");
    V::Map(out)
}

/// The ledger's tree as the row's tree has it: each record's fields in its row's order, and the
/// reservations in the order the row keeps them (the order they were made in).
fn tree(world: &World) -> V {
    let slot = world.slots.get(NAME).unwrap().try_borrow().unwrap();
    let mut encoder = Encoder::with_capacity(4096);
    slot.row.as_deref().unwrap().write_json(&mut encoder).unwrap();
    drop(slot);
    let row = world.row::<LedgerRow>(NAME).unwrap();
    let mut ledger = in_order(load::v(&serde_json::from_slice(&encoder.out).unwrap()), LedgerRow::NAMES);
    let V::Map(top) = &mut ledger else { unreachable!() };
    let accounts = top.get_mut("accounts").unwrap();
    *accounts = in_order(std::mem::replace(accounts, V::Null), Accounts::NAMES);
    let V::Map(accounts) = accounts else { unreachable!() };
    for (index, account) in accounts.values_mut().enumerate() {
        *account = in_order(std::mem::replace(account, V::Null), Account::NAMES);
        let V::Map(fields) = account else { unreachable!() };
        let reservations = fields.get_mut("reservations").unwrap();
        let made: Vec<&str> = row.accounts.of(index).reservations.keys().map(|id| &**id).collect();
        *reservations = in_order(std::mem::replace(reservations, V::Null), &made);
        let V::Map(reservations) = reservations else { unreachable!() };
        for reservation in reservations.values_mut() {
            *reservation = in_order(std::mem::replace(reservation, V::Null), Reservation::NAMES);
        }
    }
    ledger
}

/// `_digest`: the first 16 hex of the sha256 of the ledger's tree and the resources, each
/// `json.dumps(..., ensure_ascii=False)`, the resources in their order (`RESOURCE_IDS`).
fn digest(cell: &Shard<World>) -> String {
    cell.with(|world| {
        let resources = world.resources().unwrap();
        let resources = V::Map(RESOURCE_IDS.iter().map(|id| (Arc::from(*id), V::Int(resources[*id]))).collect());
        let dumps = Dumps::default().unicode();
        let text = dumps.text(&tree(world)) + &dumps.text(&resources);
        sha256_hex(text.as_bytes())[..16].to_owned()
    })
    .unwrap()
}

fn resources_of(cell: &Shard<World>) -> Vec<(String, i64)> {
    cell.with(|world| world.resources().unwrap().iter().map(|(id, value)| (id.to_string(), *value)).collect()).unwrap()
}

#[test]
fn each_operation_writes_what_the_python_rule_wrote() {
    let (_, cell) = ledger();
    operate_all(&cell);
    assert_eq!(digest(&cell), "85afdc35f4831e99");
    let mut resources = resources_of(&cell);
    resources.sort();
    assert_eq!(resources, [("food_meal_units".to_owned(), 3), ("water_ml".to_owned(), 1_300)]);
    cell.with(|world| {
        let row = world.row::<LedgerRow>(NAME).unwrap();
        assert_eq!(row.accounts.food_meal_units.reservations.get("r-1").unwrap().quantity, 1);
        assert_eq!(resources::reservation_id_was_used(world, "r-1"), Ok(true));
        assert_eq!(resources::reservation_id_was_used(world, "r-2"), Ok(true));
    })
    .unwrap();
}

#[test]
fn a_consume_of_the_whole_reservation_and_a_new_source() {
    let (_, cell) = ledger();
    let agent = operate_all(&cell);
    apply(&cell, "consume", &meal(&agent, "r-1", Some(1)), "op-7");
    let delivery = payload(&[("resource_id", text("water_ml")), ("quantity", V::Int(1))]);
    operate(&cell, OperationArg::Text("delivery"), &delivery, &text("ASM-ANOTHER-SOURCE"), "op-8").unwrap();
    assert_eq!(digest(&cell), "6b42bdea996b6a19");
    cell.with(|world| {
        let row = world.row::<LedgerRow>(NAME).unwrap();
        assert!(row.accounts.food_meal_units.reservations.is_empty());
        assert_eq!(row.accounts.water_ml.source_ids.last().map(|id| &**id), Some("ASM-ANOTHER-SOURCE"));
    })
    .unwrap();
}

fn big(digits: &str) -> V {
    load::v(&texts::big(digits))
}

/// The refused operations of the pytest file (`REFUSED`): the name, the operation, the payload (the
/// whole of it), the source, and the Python rule's text.
fn refused(agent: &str) -> Vec<(&'static str, &'static str, V, V, &'static str)> {
    let source = text(SOURCE);
    let thirty = || big("1000000000000000000000000000000");
    let reserve = |reservation: &str, quantity: V, who: &str| {
        payload(&[("resource_id", text("food_meal_units")), ("agent_id", text(who)), ("reservation_id", text(reservation)), ("quantity", quantity)])
    };
    let water = |quantity: V| payload(&[("resource_id", text("water_ml")), ("quantity", quantity)]);
    vec![
        (
            "an unknown warehouse",
            "delivery",
            exactly(&[("warehouse_id", text("elsewhere")), ("resource_id", text("water_ml")), ("quantity", V::Int(1))]),
            source.clone(),
            "unknown resource warehouse ID 'elsewhere'",
        ),
        (
            "a warehouse that is not a str",
            "delivery",
            exactly(&[("warehouse_id", V::Int(7)), ("resource_id", text("water_ml")), ("quantity", V::Int(1))]),
            source.clone(),
            "unknown resource warehouse ID 7",
        ),
        ("an unknown resource", "delivery", payload(&[("resource_id", text("soap")), ("quantity", V::Int(1))]), source.clone(), "unknown resource ID 'soap'"),
        ("a resource that is not a str", "delivery", payload(&[("resource_id", V::Null), ("quantity", V::Int(1))]), source.clone(), "unknown resource ID None"),
        ("a quantity of zero", "delivery", water(V::Int(0)), source.clone(), "resource quantity must be a positive integer"),
        ("a negative quantity past 64 bits", "loss", water(big("-1000000000000000000000000000000")), source.clone(), "resource quantity must be a positive integer"),
        ("a bool quantity", "delivery", water(V::Bool(true)), source.clone(), "resource quantity must be a positive integer"),
        ("a float quantity", "delivery", water(V::Float(1.0)), source.clone(), "resource quantity must be a positive integer"),
        ("no source", "delivery", water(V::Int(1)), V::Null, "resource events require a source_id"),
        ("no source before a bad quantity", "delivery", water(V::Int(0)), V::Null, "resource events require a source_id"),
        (
            "other payload fields",
            "delivery",
            payload(&[("resource_id", text("water_ml")), ("quantity", V::Int(1)), ("extra", V::Int(1))]),
            source.clone(),
            "resource delivery payload fields do not match",
        ),
        (
            "a missing payload field",
            "release",
            payload(&[("resource_id", text("food_meal_units")), ("reservation_id", text("r-1"))]),
            source.clone(),
            "resource release payload fields do not match",
        ),
        (
            "a loss past what is free",
            "loss",
            payload(&[("resource_id", text("food_meal_units")), ("quantity", V::Int(4))]),
            source.clone(),
            "resource loss exceeds unreserved remaining inventory",
        ),
        ("a loss past 64 bits", "loss", water(thirty()), source.clone(), "resource loss exceeds unreserved remaining inventory"),
        ("a used ID", "reserve", reserve("r-1", V::Int(1), agent), source.clone(), "resource reservation ID 'r-1' was already used"),
        (
            "a used ID before a reservation past 64 bits",
            "reserve",
            reserve("r-2", thirty(), agent),
            source.clone(),
            "resource reservation ID 'r-2' was already used",
        ),
        ("a reservation past what is free", "reserve", reserve("r-9", V::Int(9), agent), source.clone(), "resource reservation exceeds available inventory"),
        ("a reservation past 64 bits", "reserve", reserve("r-9", thirty(), agent), source.clone(), "resource reservation exceeds available inventory"),
        ("a bad reservation ID", "reserve", reserve("r 9", V::Int(1), agent), source.clone(), "resource reservation_id must be a stable ID"),
        ("someone unknown", "reserve", reserve("r 9", V::Int(1), "nobody"), source.clone(), "unknown resource Agent ID 'nobody'"),
        (
            "a bad quantity before someone unknown",
            "consume",
            reserve("r-1", V::Int(0), "nobody"),
            source.clone(),
            "resource quantity must be a positive integer",
        ),
        (
            "a reservation not there",
            "release",
            payload(&[("resource_id", text("food_meal_units")), ("agent_id", text(agent)), ("reservation_id", text("r-2"))]),
            source.clone(),
            "unknown resource reservation 'r-2'",
        ),
        ("a consume past the reservation", "consume", reserve("r-1", V::Int(2), agent), source.clone(), "resource consume exceeds the reservation"),
        ("a consume past 64 bits", "consume", reserve("r-1", thirty(), agent), source.clone(), "resource consume exceeds the reservation"),
        ("an unknown operation", "steal", water(V::Int(1)), source, "unknown resource operation 'steal'"),
    ]
}

#[test]
fn a_refusal_changes_nothing_and_has_the_python_text() {
    let (entry, cell) = ledger();
    let agent = operate_all(&cell);
    let cases = refused(&agent);
    assert_eq!(cases.len(), 25);
    for (name, operation, payload, source, wanted) in cases {
        let before = texts::canonical_of(&cell, &entry);
        assert_eq!(ledger_text(operate(&cell, OperationArg::Text(operation), &payload, &source, "op-7")), wanted, "{name}");
        assert_eq!(texts::canonical_of(&cell, &entry), before, "{name}");
    }
}

#[test]
fn someone_else_s_reservation_has_the_python_text() {
    let (_, cell) = ledger();
    operate_all(&cell);
    let other = &sorted_agents(&cell)[1];
    let release = payload(&[("resource_id", text("food_meal_units")), ("agent_id", text(other)), ("reservation_id", text("r-1"))]);
    let result = operate(&cell, OperationArg::Text("release"), &release, &text(SOURCE), "op-7");
    assert_eq!(ledger_text(result), "resource reservation belongs to a different Agent");
}

#[test]
fn a_delivery_past_64_bits_is_refused_with_nothing_changed() {
    let (entry, cell) = ledger();
    operate_all(&cell);
    let before = texts::canonical_of(&cell, &entry);
    let delivery = payload(&[("resource_id", text("water_ml")), ("quantity", big("1000000000000000000000000000000"))]);
    let result = operate(&cell, OperationArg::Text("delivery"), &delivery, &text(SOURCE), "op-7");
    assert_eq!(result, Err(Fail::Error(Error::Unsupported("a quantity past 64 bits".into()))));
    assert_eq!(texts::canonical_of(&cell, &entry), before);
}

#[test]
fn an_older_minute_is_refused() {
    let (entry, cell) = ledger();
    operate_all(&cell);
    set_minute(&cell, 4);
    let before = texts::canonical_of(&cell, &entry);
    let delivery = payload(&[("resource_id", text("water_ml")), ("quantity", V::Int(1))]);
    let result = operate(&cell, OperationArg::Text("delivery"), &delivery, &text(SOURCE), "op-7");
    assert_eq!(result, Err(Fail::Error(Error::Unsupported("the state is one the Python rule's check turned down".into()))));
    assert_eq!(texts::canonical_of(&cell, &entry), before);
}

/// One drawn operation (`ledger_sequences`): the operation, its payload and its source.
fn drawn(line: &str) -> (String, V, V) {
    let parts: Vec<&str> = line.split(' ').collect();
    let [operation, source, resource, quantity, agent, reservation] = parts[..] else { panic!("{line}") };
    let mut fields = vec![("resource_id", text(resource))];
    if quantity != "-" {
        fields.push(("quantity", V::Int(quantity.parse().unwrap())));
    }
    if agent != "-" {
        fields.push(("agent_id", text(agent)));
        fields.push(("reservation_id", text(reservation)));
    }
    (operation.to_owned(), payload(&fields), text(source))
}

/// The pytest file's `_random_sequences`: each drawn sequence on a new world, one operation a
/// minute; the digest of each outcome (the text of a refusal, else `_digest` after it), and how many
/// ran and were refused. A refusal changes nothing.
#[test]
fn random_sequences_with_refusals_are_the_python_rule_s() {
    let entry = load::entry(FILE, "ledger");
    let sequences = entry["sequences"].as_array().unwrap();
    assert_eq!(sequences.len(), 25);
    let mut outcomes = String::new();
    let (mut ran, mut refused) = (0, 0);
    for sequence in sequences {
        let cell = texts::cell(&entry);
        for (index, line) in sequence.as_array().unwrap().iter().enumerate() {
            set_minute(&cell, index as i64 + 1);
            let (operation, payload, source) = drawn(line.as_str().unwrap());
            let before = texts::canonical_of(&cell, &entry);
            match operate(&cell, OperationArg::Text(&operation), &payload, &source, &format!("op-{}", index + 1)) {
                Err(Fail::Ledger(fault)) => {
                    assert_eq!(texts::canonical_of(&cell, &entry), before, "{line}");
                    outcomes.push_str(&format!("refused {}\n", fault.text(repr_of)));
                    refused += 1;
                }
                Ok(_) => {
                    outcomes.push_str(&format!("ran {}\n", digest(&cell)));
                    ran += 1;
                }
                Err(other) => panic!("{line}: {other:?}"),
            }
        }
    }
    assert_eq!((&sha256_hex(outcomes.as_bytes())[..16], ran, refused), ("8408b20f6024ef1b", 720, 755));
}

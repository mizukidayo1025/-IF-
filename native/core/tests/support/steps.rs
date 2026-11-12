//! A world of a fixture file, its Run and its host, for the tests that call one step of a phase on
//! it (Issue #225, lane M6): the service, travel and staff steps and `_apply_effect`
//! (`propose::apply_alone`).
//!
//! `Case::of` loads a world of a file (`cases::entry`, which makes a variant again) with its Run
//! (`load::loaded`) and a host that follows the world (`load::following_host`: the floor plan of the
//! world now, a namespace a rule starts, the ledger's `world.resources`). `tick` lends a step the
//! `Tick` of the world's minute. A test reads the world as its canonical document
//! (`canonical::canonical_json`, as `WorldState.canonical_json()` writes it) and edits it as the
//! pytest edited the Python world: `edit` changes that document and loads it again, with the same
//! Run's policies. `digest` is the first 16 hex digits of the sha256 of the canonical text, the
//! value a test records of a world a step left (decision 1 of #225).

use std::rc::Rc;

use serde_json::Value;
use sheltersim_core::rules::tick::Tick;
use sheltersim_core::world::WorldCell;

use super::load::{self, Loaded};
use super::{canonical, cases, TestHost};

pub struct Case {
    pub entry: Value,
    pub loaded: Rc<Loaded>,
    pub host: TestHost,
}

impl Case {
    /// The named world of a fixture file.
    pub fn of(file: &str, name: &str) -> Self {
        Self::new(cases::entry(file, name))
    }

    pub fn new(entry: Value) -> Self {
        let loaded = Rc::new(load::loaded(&entry));
        let host = load::following_host(&entry, &loaded);
        Self { entry, loaded, host }
    }

    /// `f` with the `Tick` of the world's minute.
    pub fn tick<T>(&mut self, f: impl FnOnce(&mut Tick<'_, sheltersim_core::cell::Shard<sheltersim_core::world::World>, TestHost, std::cell::RefCell<sheltersim_core::run::Run>>) -> T) -> T {
        let minute = self.loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
        let loaded = Rc::clone(&self.loaded);
        let mut tick = Tick {
            cell: &loaded.cell,
            host: &mut self.host,
            run: &loaded.run,
            policies: loaded.policies.clone(),
            minute,
            seed: loaded.seed.clone(),
            neighbors: loaded.neighbors.clone(),
        };
        f(&mut tick)
    }

    /// `WorldState.canonical_json()` of the world now.
    pub fn bytes(&self) -> String {
        let bytes = self.loaded.cell.with(|world| canonical::canonical_json(world, &self.entry["world"]["rng_state"])).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    /// The world's canonical document now.
    pub fn doc(&self) -> Value {
        serde_json::from_str(&self.bytes()).unwrap()
    }

    /// The first 16 hex digits of the sha256 of the canonical text now.
    pub fn digest(&self) -> String {
        super::texts::sha(&self.bytes())[..16].to_owned()
    }

    /// The world with its canonical document changed by `f`, loaded again with the same Run's
    /// policies, as the pytest changed the Python world between two steps.
    pub fn edit(&mut self, f: impl FnOnce(&mut Value)) {
        let mut entry = self.entry.clone();
        let mut doc = self.doc();
        f(&mut doc);
        entry["world"] = doc;
        *self = Self::new(entry);
    }
}

/// The row of a table of a canonical document (`agents`, `facilities`, ...) with this ID.
pub fn row<'a>(doc: &'a Value, table: &str, id: &str) -> &'a Value {
    let key = if table == "agents" { None } else { Some("id") };
    doc[table]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| match key {
            Some(key) => row[key] == id,
            None => row["profile"]["id"] == id,
        })
        .unwrap_or_else(|| panic!("no {table} row {id}"))
}

/// The same row, to change.
pub fn row_mut<'a>(doc: &'a mut Value, table: &str, id: &str) -> &'a mut Value {
    let key = if table == "agents" { None } else { Some("id") };
    doc[table]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|row| match key {
            Some(key) => row[key] == id,
            None => row["profile"]["id"] == id,
        })
        .unwrap_or_else(|| panic!("no {table} row {id}"))
}

/// The inputs a fixture file's builder wrote down for a test (`inputs[name]`).
pub fn inputs(file: &str, name: &str) -> Value {
    load::data(file)["inputs"].get(name).unwrap_or_else(|| panic!("{file} has no inputs {name}")).clone()
}

/// A str of a JSON value.
pub fn s(value: &Value) -> &str {
    value.as_str().unwrap_or_else(|| panic!("not a str: {value}"))
}

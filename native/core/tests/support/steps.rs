//! A world of a fixture file, its Run and its host, for the tests that call one step of a phase on
//! it (Issue #225, lane M6): the service, travel and staff steps, `_route` (`route`) and
//! `_apply_effect` (`apply`, through `propose::apply_alone`).
//!
//! `Case::of` loads a world of a file (`cases::entry`, which makes a variant again) with its Run
//! (`load::loaded`) and a host that follows the world (`load::following_host`: the floor plan of the
//! world now, a namespace a rule starts, the ledger's `world.resources`). `tick` lends a step the
//! `Tick` of the world's minute. A test reads the world as its canonical document
//! (`canonical::canonical_json`, as `WorldState.canonical_json()` writes it) and edits it as the
//! pytest edited the Python world: `edit` changes that document and loads it again, with the same
//! Run's policies. Each step's answer and the world after it go to the case's `Answers`
//! (`answered`; `apply` and `route` add theirs), whose digest a test records again from the core
//! (decision 1 of #225).

use std::rc::Rc;

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::SectionId;
use sheltersim_core::error::Error;
use sheltersim_core::routes::Route;
use sheltersim_core::rules::proposals::{Action, Applied, Proposal};
use sheltersim_core::rules::tick::{propose, Tick};
use sheltersim_core::rules::travel::{self, Routing};
use sheltersim_core::world::WorldCell;

use super::answers::Answers;
use super::load::{self, Loaded};
use super::{canonical, cases, TestHost};

pub struct Case {
    pub entry: Value,
    pub loaded: Rc<Loaded>,
    pub host: TestHost,
    /// The answers of the steps so far, each with the world after it.
    pub answers: Answers,
}

impl Case {
    /// The named world of a fixture file.
    pub fn of(file: &str, name: &str) -> Self {
        Self::new(cases::entry(file, name))
    }

    pub fn new(entry: Value) -> Self {
        let loaded = Rc::new(load::loaded(&entry));
        let host = load::following_host(&entry, &loaded);
        Self { entry, loaded, host, answers: Answers::default() }
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

    /// The world's minute.
    pub fn minute(&self) -> i64 {
        self.loaded.cell.with(|world| world.minute_index().unwrap()).unwrap()
    }

    /// `_apply_effect` of a proposal as the pytest files made it (`SimpleNamespace`, no observed
    /// state hash): the next decision 7 minutes on (`propose::apply_alone`).
    pub fn apply(&mut self, action: Action, agent: &str, target: Option<&str>) -> Result<Applied, Error> {
        let proposal = Proposal { action, agent_id: Arc::from(agent), next_decision_at: self.minute() + 7, target_id: target.map(Arc::from), observed_state_hash: None };
        let applied = self.tick(|tick| propose::apply_alone(tick, proposal));
        self.answered(&applied);
        applied
    }

    /// A step's answer (its `Debug` text) and the world after it, added to the case's answers.
    pub fn answered<T: std::fmt::Debug>(&mut self, result: &T) {
        let text = format!("{result:?}");
        let answers = &mut self.answers;
        self.loaded.cell.with(|world| answers.add(&text, world)).unwrap();
    }

    /// The digest of the answers so far.
    pub fn hex(&self) -> String {
        self.answers.hex()
    }

    /// The controller's `_route`, on the Run's people's graph (its capacities, and the floor plan
    /// and movement section of the world now).
    pub fn route(&mut self, origin: &str, destination: &str, agent: &str) -> Option<Route> {
        let route = self.tick(|tick| {
            let graph = propose::graph(tick).unwrap();
            let holding = tick.holding().unwrap();
            let section = tick.value_section(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse).unwrap();
            let staff = holding.held("staff_operations_v1", "the world holds another document than the row's");
            let plan = tick.plan();
            let routing = Routing { graph: &graph, geo: plan.get(), movement: &section, staff: &staff };
            match tick.cell.with(|world| travel::route(world, &routing, &Arc::from(origin), &Arc::from(destination), agent)).unwrap() {
                Ok(route) => route,
                Err(error) => panic!("{error:?}"),
            }
        });
        self.answered(&route);
        route
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

    /// The world with its canonical document changed by `f`, loaded again with the same Run's
    /// policies, as the pytest changed the Python world between two steps.
    pub fn edit(&mut self, f: impl FnOnce(&mut Value)) {
        let mut entry = self.entry.clone();
        let mut doc = self.doc();
        f(&mut doc);
        entry["world"] = doc;
        let answers = std::mem::take(&mut self.answers);
        *self = Self::new(entry);
        self.answers = answers;
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

/// `(target_id, next_decision_at, committed)` of `_apply_effect`.
pub fn applied(target: Option<&str>, next_decision_at: i64, committed: bool) -> Applied {
    Applied { target: target.map(Arc::from), next_decision_at, committed }
}

/// A str of a JSON value.
pub fn s(value: &Value) -> &str {
    value.as_str().unwrap_or_else(|| panic!("not a str: {value}"))
}

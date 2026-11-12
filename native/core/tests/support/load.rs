//! The exported worlds of `tests/data/`, loaded with the crate's loader (`sheltersim_core::load`,
//! #244; first written here as a test-only loader for #152 lane P10-1b and #225).
//!
//! A fixture file under `tests/data/` holds named worlds (`{"worlds": {name: entry}}`), which
//! `sheltersim.core.world_export.export_world` exports and `benchmarks/core_fixtures.py` writes
//! (the builders `tests/support/core_fixtures.py` registers): each entry has the world's canonical
//! document at its minute, the slots its core owns, what the Run read of the controller
//! (`RunCore.policies_document`) and the Python-parsed sections of its configs. `entry` reads one;
//! `fixture` is the small world of `tests/support/core_tick.py` at minute 0 (`tick_core.json`'s
//! `small_replay`). The functions of the crate's loader are here as the tests call them: an entry
//! a test reads is well formed, so each panics where the crate's gives an error.
//!
//! `host` is a `TestHost` with the entry's sections and floor plan; `following_host` also does to
//! the loaded world what the PyO3 crate's host does to the Python world it owns (`Following`).

use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::config::Fault;
use sheltersim_core::error::Error;
use sheltersim_core::events::EventRow;
use sheltersim_core::geometry::Geo;
use sheltersim_core::load::{self as core_load, FloorPlans, Sections};
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::run::held::Obj;
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell};

pub use sheltersim_core::load::Loaded;

/// A fixture file of `tests/data/` (`<name>.json`).
pub fn data(name: &str) -> Value {
    let path = format!("{}/tests/data/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_str(&raw).unwrap()
}

/// The named world of a fixture file, as `export_world` wrote it (with the keys its builder added).
pub fn entry(file: &str, world: &str) -> Value {
    let mut data = data(file);
    let names: Vec<String> = data["worlds"].as_object().unwrap_or_else(|| panic!("{file} has no worlds")).keys().cloned().collect();
    data["worlds"].get_mut(world).unwrap_or_else(|| panic!("{file} has no world {world}: {names:?}")).take()
}

/// The small world of the core's tick test (`tick_core.json`'s `small_replay`).
pub fn fixture() -> Value {
    entry("tick_core", "small_replay")
}

/// `load::value`: a JSON value as the core's `V`.
pub fn v(value: &Value) -> V {
    core_load::value(value).unwrap()
}

/// `load::event`: an event of the queue (`ScheduledEvent.to_dict()`).
pub fn event(value: &Value) -> EventRow {
    core_load::event(value).unwrap()
}

/// `load::put_namespace`: `model_state[name] = value` in the World the PyO3 crate's core holds.
pub fn put_namespace(world: &mut World, name: &str, value: V) {
    core_load::put_namespace(world, name, value).unwrap_or_else(|error| panic!("{error}"))
}

/// `load::world`: the World of a canonical document, with a slot for each owned name.
pub fn world(document: &Value, slots: &Value) -> World {
    core_load::world(document, slots).unwrap_or_else(|error| panic!("{error}"))
}

/// `load::fault`: a fault as the exporter writes it, `[class, text]`.
pub fn fault(value: &Value) -> Fault {
    core_load::fault(value).unwrap()
}

/// `load::obj`: an object as `RunCore.policies_document` wrote it.
pub fn obj(value: &Value) -> Obj {
    core_load::obj(value).unwrap()
}

/// `load::loaded`: the fixture's world and Run at its minute.
pub fn loaded(fixture: &Value) -> Loaded {
    core_load::loaded(fixture).unwrap_or_else(|error| panic!("{error}"))
}

/// A `TestHost` whose Python-parsed sections are the exported world's (`sections::fill`) and whose
/// floor plan is the world's as exported (`sections::floor_plan`; none where `Geo::of` refuses).
pub fn host(fixture: &Value) -> super::TestHost {
    let mut host = super::TestHost::default();
    super::sections::fill(&mut host, fixture);
    host.plan = super::sections::floor_plan(fixture).ok();
    host
}

/// `host` on the loaded world, for a Run's ticks (#214): it builds the floor plan of the world as it
/// is at each call (`Following::floor_plan`) and finds the records of the world's log
/// (`Following::find_record`, #225 lane M9). It writes nothing of the world: the core writes
/// `world.resources` and the namespaces it starts itself, and only tells the host (#244, decision 2).
pub fn following_host(fixture: &Value, loaded: &Rc<Loaded>) -> super::TestHost {
    let mut host = host(fixture);
    let sections = Sections::of_entry(fixture).unwrap_or_else(|error| panic!("{error}"));
    host.world = Some(Following { loaded: Rc::clone(loaded), sections, plans: FloorPlans::default() });
    host
}

/// What a host reads of the world it owns, as the PyO3 crate's `PyHost` reads the Python world, on
/// a loaded World (#214).
pub struct Following {
    loaded: Rc<Loaded>,
    /// The sections of the export, which the plan is built from.
    sections: Sections,
    /// The plan last built, for the overlay's key it was built for.
    plans: FloorPlans,
}

impl Following {
    /// `Geo::of`: the plan of the world's overlay (`geometry_overlay_v1`) now, built again
    /// (`FloorPlans::plan`) when the overlay's facility moves or partitions change (its
    /// `OverlayKey`), so a tick that moves a facility walks on the new plan. An error is the refusal
    /// `Geo::of` raises.
    pub fn floor_plan(&mut self) -> Result<Arc<Geo>, Error> {
        let overlay = self.loaded.cell.with(|world| core_load::overlay(world))??;
        self.plans.plan(&self.sections, overlay.as_ref())
    }

    /// How many times `floor_plan` built the plan.
    pub fn builds(&self) -> usize {
        self.plans.builds()
    }

    /// `event_log.find` on the world's log now (`load::find_record`).
    pub fn find_record(&self, event_id: &str) -> Result<Option<Logged>, Error> {
        self.loaded.cell.with(|world| core_load::find_record(world, event_id))?
    }
}

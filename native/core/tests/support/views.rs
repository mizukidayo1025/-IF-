//! The views the update and the propose phases make of a loaded world, and an exact text of a view
//! (Issue #225, lane M7: `world_view_reads.rs` and `world_view_run.rs`).
//!
//! `after_movement` is `_view_after_movement` (`tick::update::view_after_movement`), built with
//! the Run's cache of what it read (`Run::views`); `for_proposals` is `_view_for_proposals`
//! (`tick::propose::view_for_proposals`): the update phase's view kept in the Run's scratch read
//! again, or a new view, with its audit hash. `fresh` is a view built with a new cache, as
//! `support.world.view_of` of the pytest files made one (nothing of an earlier view kept).
//! `exact` writes every field a reader of a view gets (the floats by their bits, `{:?}` being
//! the shortest text that reads back to the same bits), as `_exact` of `test_world_view_row.py`
//! did of the Python objects.

use std::fmt::Write as _;
use std::sync::Arc;

use sheltersim_core::rules::tick::propose::{self, Hash};
use sheltersim_core::rules::tick::update;
use sheltersim_core::rules::tick::Prior;
use sheltersim_core::rules::world_view::{self as view, ViewCache, ViewData};
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell};

use super::load::Loaded;
use super::ticks::tick_at;
use super::TestHost;

fn minute_of(loaded: &Loaded) -> i64 {
    loaded.cell.with(|world| world.minute_index().unwrap()).unwrap()
}

/// `_view_after_movement` at the world's minute.
pub fn after_movement(loaded: &Loaded, host: &mut TestHost) -> ViewData {
    let minute = minute_of(loaded);
    update::view_after_movement(&mut tick_at(loaded, host, minute)).unwrap_or_else(|error| panic!("{error:?}"))
}

/// `_view_for_proposals` at the world's minute: the view, and its audit hash.
pub fn for_proposals(loaded: &Loaded, host: &mut TestHost) -> (Arc<ViewData>, String) {
    let minute = minute_of(loaded);
    let data = propose::view_for_proposals(&mut tick_at(loaded, host, minute)).unwrap_or_else(|error| panic!("{error:?}"));
    let hash = loaded.run.borrow().scratch.get::<Hash>().map(|hash| hash.0.to_string()).expect("the view's hash");
    (data, hash)
}

/// The update phase's view handed on to the propose phase (`_view_before_proposals`).
pub fn hand_on(loaded: &Loaded, data: ViewData) {
    let minute = minute_of(loaded);
    let mut run = loaded.run.borrow_mut();
    if run.scratch.minute() != Some(minute) {
        run.scratch.begin(minute);
    }
    run.scratch.put(Prior::View(Arc::new(data)));
}

/// What the update phase handed on, if anything.
pub fn handed_on(loaded: &Loaded) -> Option<Arc<ViewData>> {
    match loaded.run.borrow().scratch.get::<Prior>() {
        Some(Prior::View(data)) => Some(data.clone()),
        Some(Prior::Python { .. }) => panic!("a view of Python's"),
        None => None,
    }
}

/// A view of the world built with a new cache, with its audit hash if `hashed`. The Run's cache is
/// left as it was.
pub fn fresh(loaded: &Loaded, host: &mut TestHost, hashed: bool) -> (ViewData, Option<String>) {
    let kept = std::mem::take(&mut loaded.run.borrow_mut().views);
    let data = after_movement(loaded, host);
    loaded.run.borrow_mut().views = kept;
    let hash = hashed.then(|| loaded.cell.with(|world| view::state_hash(&data, world, None)).unwrap().unwrap());
    (data, hash)
}

/// A new cache for the Run, as if it read nothing yet.
pub fn forget(loaded: &Loaded) {
    loaded.run.borrow_mut().views = ViewCache::default();
}

/// Every field a reader of the view gets, with its hash: the same text for the same view only.
pub fn exact(data: &ViewData, hash: Option<&str>) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "minute {} hash {hash:?}", data.minute_index);
    let _ = writeln!(out, "service {} autonomy {} context {}", data.resource_service_enabled, data.autonomy_enabled, data.has_population_context);
    for agent in &data.agents {
        let needs = &agent.needs;
        let _ = writeln!(
            out,
            "agent {:?} group {:?} zone {:?} mobility {} support_need {:?} sensitivity {:?} rng_key {:?} activity {} target {:?} \
             needs {:?} {:?} {:?} {:?} stress {:?} chronic {:?} trust {:?} next {} queue {:?} {:?} {:?} {} \
             request {:?} {:?} {:?} {} cooldown {} query {:?} belief {:?} sources {:?} body {:?} person {:?}",
            agent.id,
            agent.group_id,
            data.zone_of(agent).id,
            agent.mobility.text(),
            agent.support_need,
            agent.stress_sensitivity,
            agent.rng_key,
            agent.activity.text(),
            agent.target_id,
            needs.toilet_urgency,
            needs.hunger,
            needs.fatigue,
            needs.hydration_need,
            agent.stress,
            agent.chronic_distress,
            agent.trust,
            agent.next_decision_at,
            agent.queue_task_id,
            agent.queue_kind,
            agent.queue_target_id,
            agent.queue_wait_minutes,
            agent.support_request_id,
            agent.support_request_status.map(|status| status.text()),
            agent.support_assigned_staff_id,
            agent.support_wait_minutes,
            agent.support_cooldown_remaining_minutes,
            agent.information_query_pressure,
            agent.misinformation_belief_pressure,
            agent.misinformation_effect_sources,
            agent.body,
            agent.person,
        );
    }
    for zone in &data.zones {
        let _ = writeln!(out, "zone {zone:?}");
    }
    for facility in &data.facilities {
        let _ = writeln!(out, "facility {facility:?}");
    }
    let ids = |indexes: &[usize]| -> Vec<&str> { indexes.iter().map(|index| &*data.agents[*index].id).collect() };
    for (zone, members) in data.zone_members() {
        let _ = writeln!(out, "zone members {zone:?} {:?}", ids(&members));
    }
    for (group, members) in data.group_members() {
        let _ = writeln!(out, "group members {group:?} {:?}", ids(&members));
    }
    out
}

// ---- Writing the world as a Python rule or a test would ------------------------------------------

/// A namespace of `model_state` (or a field's slot) as a `V`, from its row's JSON.
pub fn namespace(world: &World, name: &str) -> V {
    let slot = world.slots.get(name).unwrap_or_else(|| panic!("no {name}")).try_borrow().unwrap();
    let mut encoder = sheltersim_core::json::Encoder::with_capacity(1024);
    slot.row.as_deref().unwrap_or_else(|| panic!("{name} has no row")).write_json(&mut encoder).unwrap();
    super::load::v(&serde_json::from_slice(&encoder.out).unwrap())
}

/// `model_state[name]` written by `edit` (the namespace's row again, with a new stamp, as a write
/// through its mirror gives it).
pub fn rewrite(world: &mut World, name: &str, edit: impl FnOnce(&mut V)) {
    let mut value = namespace(world, name);
    edit(&mut value);
    super::load::put_namespace(world, name, value);
    world.slots.get(name).unwrap().try_borrow_mut().unwrap().touch();
}

/// The map at `path` below `value`, made where it is missing (`setdefault(key, {})`).
pub fn at<'a>(value: &'a mut V, path: &[&str]) -> &'a mut V {
    let mut node = value;
    for key in path {
        let V::Map(map) = node else { panic!("not a map at {key}") };
        node = map.entry(Arc::from(*key)).or_insert_with(|| V::Map(Default::default()));
    }
    node
}

/// `value[path...][key] = item`.
pub fn set(value: &mut V, path: &[&str], key: &str, item: V) {
    match at(value, path) {
        V::Map(map) => {
            map.insert(Arc::from(key), item);
        }
        other => panic!("not a map: {other:?}"),
    }
}

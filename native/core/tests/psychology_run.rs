//! The psychology of a Run's ticks in the core, against what the deleted Python rules gave (Issue
//! #225, lane M11; moved from `tests/integration/test_native_kernels.py` and
//! `test_psychology_row.py`).
//!
//! The world is the generated Scenario the Project screen makes, at minute 0 with the Run's
//! policies (`generated_texts.json`'s `generated`); the core runs its ticks as `TickEngine.step`
//! does (`support/ticks.rs`), which pass the worlds of `run_scenario` (`generated_run.rs`).
//!
//! - The psychology step's deltas at each of the Run's 360 minutes, as the update phase finds the
//!   world: the sha256 of `json.dumps` of each delta's fields, recorded once from the deleted Python
//!   step `_psychology_update` (at 45a9bc9). `Dumps` writes the same text, so the digest is the
//!   original one.
//! - The long-term distress and the world after 40 ticks: the sha256 of `json.dumps` of the
//!   namespace's people and of `canonical_json()`, recorded once from the deleted rules of
//!   `domain/psychological_state.py` and `domain/distress_monitor.py`. The core writes the same
//!   bytes (`support/canonical.rs`, with the `rng_state` the engine gives), so this digest is the
//!   original one too.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use serde_json::Value;
use sha2::{Digest, Sha256};
use sheltersim_core::checkpoint::hex;
use sheltersim_core::host::Phase;
use sheltersim_core::rules::after_tick::Memo;
use sheltersim_core::rules::psychology::{self, Delta};
use sheltersim_core::rules::tick::{self, update};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::psychological_distress::{DistressRow, NAME};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell, METRIC_STATE};

use support::canonical::canonical_json;
use support::dumps::dumps;
use support::load::{self, Loaded};
use support::ticks::{self, check_after_tick, tick_at, NoPython};
use support::{cases, TestHost};

/// `PSYCHOLOGY_DELTAS` of `test_native_kernels.py`.
const PSYCHOLOGY_DELTAS: &str = "842c8f23bf0c3e3ce2a8b971ca3bc742cc6c263cedaa52918c04e2e6d3ba7d9d";
/// `ANSWERS["ticks"]` of `test_psychology_row.py`.
const TICKS: &str = "fe85fda26cad60572bea948b335dadb049ca64df5b21b88737d44d4785171ae9";

fn generated() -> (Value, Rc<Loaded>, TestHost) {
    let entry = cases::entry("generated_texts", "generated");
    let loaded = Rc::new(load::loaded(&entry));
    let host = load::following_host(&entry, &loaded);
    (entry, loaded, host)
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// `[getattr(delta, name) for name in AgentStateDelta.__dataclass_fields__]`.
fn delta_fields(delta: &Delta) -> V {
    let mut fields = vec![
        text(&delta.agent_id),
        V::Int(delta.minute_index),
        V::Float(delta.toilet_urgency),
        V::Float(delta.hunger),
        V::Float(delta.fatigue),
        V::Float(delta.stress),
        V::Float(delta.trust),
        V::Float(delta.hydration_need),
    ];
    match delta.body {
        Some(body) => fields.extend(body.iter().map(|value| V::Float(*value))),
        None => fields.extend(std::iter::repeat_n(V::Null, psychology::BODY_NAMES.len())),
    }
    V::List(fields)
}

/// `test_the_psychology_kernel_gives_the_deltas_of_the_deleted_python_step`: before each tick's
/// update phase, the step on the view `_view_after_movement` makes then. In this Run the movement
/// replans no trip, so it moves only the points of the walks, which the step doesn't read.
#[test]
fn the_psychology_step_gives_the_deltas_of_the_deleted_python_step() {
    let (_, loaded, mut host) = generated();
    let (mut memo, mut checks) = (Memo::default(), NoPython::default());
    let before: Vec<Phase> = Phase::ORDER.into_iter().filter(|phase| matches!(phase, Phase::DispatchDueEvents | Phase::CompleteScheduledTasks)).collect();
    let rest: Vec<Phase> = Phase::ORDER.into_iter().filter(|phase| !before.contains(phase)).collect();
    let mut digest = Sha256::new();
    for minute in 0..360 {
        let mut tick = tick_at(&loaded, &mut host, minute);
        if let Err(failed) = tick::run_phases(&mut tick, &before) {
            panic!("minute {minute}: {:?} {:?}", failed.label, failed.error);
        }
        let view = update::view_after_movement(&mut tick).unwrap();
        assert_eq!(view.minute_index, minute);
        let model = psychology::model_of(&loaded.policies.psychology).unwrap();
        let params = psychology::params(&loaded.policies.psychology, model, Some(view.resource_service_enabled), false).unwrap();
        let result = loaded.cell.with(|world| psychology::update(world, &view, &params, minute, false)).unwrap().unwrap();
        for delta in result.deltas.iter() {
            digest.update(dumps(&delta_fields(delta)).as_bytes());
            digest.update(b"\n");
        }
        if let Err(failed) = tick::run_phases(&mut tick, &rest) {
            panic!("minute {minute}: {:?} {:?}", failed.label, failed.error);
        }
        loaded.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute + 1)).unwrap();
        check_after_tick(&loaded, &mut host, &mut memo, &mut checks).unwrap();
    }
    let replans = loaded.cell.with(|world| metric(world, "movement_replan_count")).unwrap();
    assert_eq!(replans, 0.0);
    assert_eq!(hex(&digest.finalize()), PSYCHOLOGY_DELTAS);
}

fn metric(world: &World, name: &str) -> f64 {
    world.row::<MetricRow>(METRIC_STATE).unwrap().get(name).copied().unwrap_or(0.0)
}

/// The people of `psychological_distress_v1` as the tree the namespace builds of its row: each
/// person's record with its fields in their order.
fn distress_people(world: &World) -> V {
    let row = world.row::<DistressRow>(NAME).unwrap();
    V::Map(
        row.agents
            .iter()
            .map(|(id, record)| {
                let fields = [
                    ("baseline_distress", V::Float(record.baseline_distress)),
                    ("long_term_distress", V::Float(record.long_term_distress)),
                    ("daily_acute_sum", V::Float(record.daily_acute_sum)),
                    ("daily_high_distress_minutes", V::Int(record.daily_high_distress_minutes)),
                    ("daily_observed_minutes", V::Int(record.daily_observed_minutes)),
                    ("completed_days", V::Int(record.completed_days)),
                ];
                (id.clone(), V::Map(fields.into_iter().map(|(name, value)| (Arc::from(name), value)).collect()))
            })
            .collect(),
    )
}

/// The `rng_state` a `TickEngine` gives the world it steps: its `CounterRNG`'s state, which the
/// world exported at minute 0, before any engine, does not have yet.
fn engine_rng_state(entry: &Value) -> Value {
    serde_json::json!({"algorithm": "sha256-keyed-v1", "seed": entry["world"]["seed"]})
}

/// `test_ticks_make_the_world_of_the_deleted_rules`: 40 ticks, then the namespace's people and the
/// world's canonical JSON.
#[test]
fn ticks_make_the_world_of_the_deleted_rules() {
    let (entry, loaded, mut host) = generated();
    ticks::run(&loaded, &mut host, 40, &mut |_, _| {});
    let rng_state = engine_rng_state(&entry);
    let (people, canonical) = loaded.cell.with(|world| (dumps(&distress_people(world)), canonical_json(world, &rng_state))).unwrap();
    let mut digest = Sha256::new();
    for text in [people.as_bytes(), &canonical] {
        digest.update(text);
        digest.update(b"\n");
    }
    assert_eq!(hex(&digest.finalize()), TICKS);
}

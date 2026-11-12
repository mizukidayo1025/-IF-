//! The metrics phase (`_run_metrics_phase` of `experiments/phases/metrics.py`): the minute's
//! metrics, to the sink the host says (`Host::metric_sink`), and the open actions closed at the
//! Run's last minute (`finalize_active_actions`, with the Run's actions).

use crate::config::movement::Movement;
use crate::config::SectionId;
use crate::error::Error;
use crate::host::{Host, MetricSink};
use crate::rules::action_lifecycle;
use crate::rules::metrics::{self, Captured, Inputs, Requests};
use crate::rules::overlay::PLAN_UNREAD;
use crate::rules::staff::WaitRoutes;
use crate::world::WorldCell;

use super::arbitrate::Staff;
use super::read::is_refusal;
use super::{Labelled, RunCell, Step, Tick};

pub const CAPTURE: &str = "metrics.capture";
pub const FINALIZE: &str = "action_lifecycle.finalize_active_actions";

/// The metrics phase.
pub fn metrics<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> Step<(), H::Error> {
    let minute = tick.minute;
    match tick.host.metric_sink().bare()? {
        // The core's sample goes to the recorder through the host. A refusal leaves the sample to
        // the sink, which says what is wrong.
        MetricSink::Recorder => match capture(tick) {
            Err(error) if is_refusal(&error).is_some() => {
                crate::census::caught(CAPTURE, is_refusal(&error).unwrap_or_default());
                tick.host.capture_by_sink(minute).bare()?;
            }
            other => other.bare()?,
        },
        MetricSink::Sink => tick.host.capture_by_sink(minute).bare()?,
    }
    // `_finalize_actions_at_end` reads the duration again, and acts at the Run's last minute.
    let duration = tick.cell.with(|world| world.duration_minutes()).map_err(H::Error::from).bare()?;
    if duration.map_or(true, |duration| minute + 1 >= duration) {
        let done = tick.run.with(|run| tick.cell.with(|world| action_lifecycle::finalize(world, &mut run.actions, minute)));
        done.and_then(|done| done).and_then(|done| done).map_err(H::Error::from).at(FINALIZE)?;
    }
    Ok(())
}

/// `capture_of_run`: the core's sample of the open requests the world has, first without the
/// waits' routes and again with them where the waits need them.
fn capture<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    let staff = Staff::of(&holding)?;
    let staff_inputs = staff.inputs(None);
    let first = Inputs { sample: tick.minute, requests: Requests::Unsettled(None), staff: &staff_inputs, routes: None };
    if metrics::capture(tick.cell, tick.host, &first)? == Captured::Taken {
        return Ok(());
    }
    tick.host.read_queue_rows()?;
    let loads_staff = holding.held("staff_operations_v1", "the world holds another document than the row's");
    let plan = tick.plan();
    let movement = tick.value_section(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
    let support_minutes = tick.value_section(&holding, SectionId::SupportTaskMinutes, |configs| &configs.support_task_minutes, crate::config::staff::support_task_minutes)?;
    let routes = WaitRoutes { loads_staff: &loads_staff, geo: plan.get(), movement: &movement, support_minutes: &support_minutes };
    let again = Inputs { sample: tick.minute, requests: Requests::Unsettled(None), staff: &staff_inputs, routes: Some(&routes) };
    let found = metrics::capture(tick.cell, tick.host, &again);
    let found = match (found, plan.0) {
        (Err(error), Err(read)) if is_refusal(&error).as_deref() == Some(PLAN_UNREAD) => return Err(read),
        (found, _) => found?,
    };
    match found {
        Captured::Taken => Ok(()),
        Captured::NeedsRoutes => Err(Error::Runtime("the waits asked for their routes again".to_owned()).into()),
    }
}

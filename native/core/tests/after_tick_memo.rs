//! The memo of the end-of-tick check on the worlds of a Run (Issue #225, lane M11; moved from
//! `tests/integration/test_after_tick_kernel.py`).
//!
//! The pytest file took the worlds of a Run of the generated Scenario every 20 minutes; here the core
//! runs the same world's ticks (`generated_texts.json`'s `generated`, `support/ticks.rs`, which pass
//! the worlds of `run_scenario`) and checks it at the same minutes. A world is "primed" as the
//! pytest file's `_primed` primed it: a new memo that the check (`after_tick::check`, the kernel of
//! `WorldState.validate_after_tick`) has passed once. What stays in pytest is what compares the
//! kernel with the Python check (a yes of the kernel means the Python rules pass), the Python
//! writes that reach the rows, and the patches of the Python rules.

mod support;

use std::rc::Rc;
use std::sync::Arc;

use sheltersim_core::entities::{Activity, RequestRow, RequestStatus};
use sheltersim_core::rules::after_tick::Memo;
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::load::{self, Loaded};
use support::ticks::{self, check_after_tick, NoPython};
use support::{cases, TestHost};

fn generated() -> (Rc<Loaded>, TestHost) {
    let entry = cases::entry("generated_texts", "generated");
    let loaded = Rc::new(load::loaded(&entry));
    let host = load::following_host(&entry, &loaded);
    (loaded, host)
}

/// The check of the world as it is now, with this memo.
fn check(loaded: &Loaded, host: &mut TestHost, memo: &mut Memo) -> Result<(), sheltersim_core::error::Error> {
    check_after_tick(loaded, host, memo, &mut NoPython::default())
}

/// A new memo the check passed once on the world as it is now (`_primed`).
fn primed(loaded: &Loaded, host: &mut TestHost) -> Memo {
    let mut memo = Memo::default();
    check(loaded, host, &mut memo).unwrap();
    memo
}

fn is_settled(row: &RequestRow) -> bool {
    matches!(row.status, RequestStatus::Resolved | RequestStatus::Unresolved)
}

/// The requests whose rows were written since the last passed check (`written_requests`).
fn written(world: &World) -> Vec<Arc<str>> {
    let rows = world.rows().unwrap();
    let table = &rows.support_requests;
    table.marked().iter().map(|slot| table.slots().find(|(_, at)| at == slot).unwrap().0.clone()).collect()
}

/// `test_the_memo_moves_on_to_the_minute_it_checked`: the world of minute 20, its minute moved on.
#[test]
fn the_memo_moves_on_to_the_minute_it_checked() {
    let (loaded, mut host) = generated();
    ticks::run(&loaded, &mut host, 20, &mut |_, _| {});
    let mut memo = primed(&loaded, &mut host);
    assert_eq!(memo.minute, 20);
    loaded.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(21)).unwrap();
    check(&loaded, &mut host, &mut memo).unwrap();
    assert_eq!(memo.minute, 21);
}

/// `test_a_new_request_is_read_and_held`: a settled request copied under a new key is read and
/// held, with no mark left; a write to it marks its slot, and the check turns it down.
#[test]
fn a_new_request_is_read_and_held() {
    let (loaded, mut host) = generated();
    ticks::run(&loaded, &mut host, 20, &mut |_, _| {});
    let mut memo = primed(&loaded, &mut host);
    let held = memo.requests.len();
    loaded
        .cell
        .with(|world| {
            let mut rows = world.rows_mut().unwrap();
            let settled = rows.support_requests.iter().map(|(_, row)| row).find(|row| is_settled(row)).expect("a settled request").clone();
            rows.support_requests.insert(Arc::from("probe-request"), RequestRow { id: Arc::from("probe-request"), ..settled });
        })
        .unwrap();
    check(&loaded, &mut host, &mut memo).unwrap();
    assert_eq!(memo.requests.len(), held + 1);
    assert!(loaded.cell.with(|world| written(world)).unwrap().is_empty());
    loaded.cell.with(|world| world.rows_mut().unwrap().support_requests.get_mut("probe-request").unwrap().requested_minute = -1).unwrap();
    assert_eq!(loaded.cell.with(|world| written(world)).unwrap(), [Arc::from("probe-request")]);
    assert!(check(&loaded, &mut host, &mut memo).is_err());
}

/// `_reopened`: the first settled request, before an open one in the table, of someone idle with no
/// other open request, pending again as the rules want it (the timeout its worker left in the queue,
/// or the Run's end). Its key, or None.
fn reopen(world: &World) -> Option<Arc<str>> {
    let duration = world.duration_minutes().unwrap();
    let mut rows = world.rows_mut().unwrap();
    let requests: Vec<RequestRow> = rows.support_requests.iter().map(|(_, row)| row.clone()).collect();
    let open_people: Vec<&Arc<str>> = requests.iter().filter(|row| !is_settled(row)).map(|row| &row.agent_id).collect();
    let last_open = requests.iter().rposition(|row| !is_settled(row))?;
    let item = requests[..last_open].iter().find(|row| {
        let agent = rows.agents.get(&row.agent_id).unwrap();
        is_settled(row) && !open_people.contains(&&row.agent_id) && agent.support_request_id.is_none() && agent.activity == Activity::Idle
    })?;
    let (id, agent_id) = (item.id.clone(), item.agent_id.clone());
    let queue = world.queue().unwrap();
    let kept = queue.ordered().iter().filter_map(|(_, serial)| queue.slot(*serial).row.clone()).find(|event| {
        let V::Map(payload) = &event.payload else { return false };
        &*event.event_type == "support_request_timeout" && matches!(payload.get("request_id"), Some(V::Str(request)) if *request == id)
    });
    drop(queue);
    let request = rows.support_requests.get_mut(&id).unwrap();
    request.status = RequestStatus::Pending;
    request.resolved_minute = None;
    request.assigned_staff_id = None;
    request.task_id = None;
    request.timeout_minute = kept.map_or(duration, |event| event.due_minute);
    let agent = rows.agents.get_mut(&agent_id).unwrap();
    agent.support_request_id = Some(id.clone());
    agent.activity = Activity::SeekingHelp;
    Some(id)
}

/// `test_a_reopened_request_comes_back_in_the_order_of_the_dict`: at the first minute of 20, 40, ...
/// with such a request, the check holds the open requests in the order of the table, the reopened
/// one in its place.
#[test]
fn a_reopened_request_comes_back_in_the_order_of_the_table() {
    let (loaded, mut host) = generated();
    let duration = loaded.cell.with(|world| world.duration_minutes().unwrap()).unwrap();
    for _ in 0..duration / 20 {
        ticks::run(&loaded, &mut host, 20, &mut |_, _| {});
        let mut memo = primed(&loaded, &mut host);
        let Some(id) = loaded.cell.with(|world| reopen(world)).unwrap() else {
            continue;
        };
        assert!(loaded.cell.with(|world| written(world)).unwrap().contains(&id));
        let wanted: Vec<Arc<str>> = loaded.cell.with(|world| world.rows().unwrap().support_requests.iter().filter(|(_, row)| !is_settled(row)).map(|(key, _)| key.clone()).collect()).unwrap();
        assert!(wanted.contains(&id));
        check(&loaded, &mut host, &mut memo).unwrap();
        assert_eq!(memo.open, wanted);
        return;
    }
    panic!("no world of the Run has a request to reopen");
}

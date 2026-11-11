//! Lane P8 e1: `_apply_effect` (`rules::proposals`) in the core, on worlds built here: the activity
//! of `wait` and `rest` and the bed given up, a social action's event, a request with the
//! information model's record, the LLM's hooks through a host that writes the world (rule R6), the
//! refusals before any write, and a resource action without a service.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::entities::{Activity, AgentRow, Entities, Mobility, NeedRow, ProfileRow, RequestStatus};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::host::{Contract, Decision, DecisionSink, Host, Llm, Phase, Stage, Trace, TraceLevel, TxnKind};
use sheltersim_core::row::{IdMap, Row, ValueRow};
use sheltersim_core::rules::kit::{Events, Held};
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::rules::proposals::{self as proposals, Action, Applied, Information, Inputs, Proposal, Seek, THRESHOLD_UNREAD};
use sheltersim_core::rules::support::{request_id_of, RequestService, NAMESPACE_IS_NONE};
use sheltersim_core::state::information::{BeliefRow, CountersRow, InformationRow, SupportRequestRow, NAME as INFORMATION};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Snapshot, World, WorldCell, ID_COUNTERS, METRIC_STATE, MODEL_CONFIGS};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (text(key), value.clone())).collect())
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, zone: &str) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text(zone),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(),
        stress: 0.5,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn slot(world: &mut World, name: &str, place: Place, row: Box<dyn Row>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
}

fn belief(message: &str, status: &str) -> BeliefRow {
    BeliefRow {
        message_id: text(message),
        belief: text(status),
        source: text("a2"),
        last_contact: 3,
        last_event_id: text("ev-origin"),
        exposed: true,
        belief_strength: 0.5,
        share_propensity: 0.5,
        correction_exposed: false,
        correction_accepted: false,
        ever_misbelieved: false,
        ever_forgotten: false,
        last_correction_accepted: false,
    }
}

/// a1 has heard m2 and m1 and believes m3; a2 holds no claim; a3 has no beliefs at all.
fn information() -> InformationRow {
    let mut beliefs = IdMap::default();
    let mut held = IdMap::default();
    for (message, status) in [("m2", "heard"), ("m3", "believed"), ("m1", "heard")] {
        held.insert(text(message), belief(message, status));
    }
    beliefs.insert(text("a1"), held);
    beliefs.insert(text("a2"), IdMap::default());
    let mut pressure = IdMap::default();
    pressure.insert(text("a1"), 0.75);
    pressure.insert(text("a2"), 0.25);
    InformationRow {
        schema_version: 2,
        messages: IdMap::default(),
        beliefs,
        contacts: IdMap::default(),
        query_pressure: pressure,
        support_requests: Vec::new(),
        counters: CountersRow { contact_attempts: 0, contact_deliveries: 0, correction_deliveries: 0, support_requests: 0 },
    }
}

/// Minute 7 of 100, with its queue and log and `id_counters` at `event: 10`: a1 and a2 in z0, a3
/// in z1, and the information state; a1 holds a bed in the overlay.
fn world() -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.duration_minutes = Fit::Ok(100);
        scalars.minute_index = Fit::Ok(7);
    }
    let mut entities = Entities::default();
    entities.agents.insert(text("a1"), agent("a1", "z0"));
    entities.agents.insert(text("a2"), agent("a2", "z0"));
    entities.agents.insert(text("a3"), agent("a3", "z1"));
    *world.entities.get_mut() = entities;
    slot(&mut world, ID_COUNTERS, Place::Field, Box::new(ValueRow(map(&[("event", V::Int(10))]))));
    slot(&mut world, METRIC_STATE, Place::Field, Box::new(MetricRow::default()));
    slot(&mut world, MODEL_CONFIGS, Place::Field, Box::new(ValueRow(V::Map(IndexMap::new()))));
    slot(&mut world, INFORMATION, Place::Namespace, Box::new(information()));
    let reservation = map(&[("agent_id", V::Str(text("a1"))), ("room_id", V::Str(text("room-1"))), ("bed_index", V::Int(0))]);
    slot(&mut world, "geometry_overlay_v1", Place::Namespace, Box::new(ValueRow(map(&[("bed_reservations", map(&[("a1", reservation)]))]))));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn proposal(action: Action, agent_id: &str) -> Proposal {
    Proposal { action, agent_id: text(agent_id), next_decision_at: 15, target_id: None, observed_state_hash: Some(text(&"a".repeat(64))) }
}

fn service() -> RequestService {
    RequestService { unresolved_after_minutes: Some(30), cooldown_minutes: Some(15), policy_version: Some(text("staff-support-v1")) }
}

fn seek(threshold: Option<f64>) -> Seek {
    Seek { service: service(), shared: Held::Absent, information: Some(Information { threshold, held: Held::Held }) }
}

fn inputs(seek: Option<Seek>) -> Inputs<'static> {
    Inputs { overlay: Held::Held, metric_state_held: true, social_minutes: Some(5), sleep: None, trips: None, seek }
}

fn metric(world: &World, name: &str) -> f64 {
    world.metric_state().unwrap().get(name).copied().unwrap_or(0.0)
}

fn reservations(world: &World) -> Vec<Arc<str>> {
    let overlay = world.row::<ValueRow>("geometry_overlay_v1").unwrap();
    let V::Map(fields) = &overlay.0 else { panic!("no map") };
    let Some(V::Map(held)) = fields.get("bed_reservations") else { panic!("no reservations") };
    held.keys().cloned().collect()
}

fn person(world: &World, agent_id: &str) -> (Activity, Option<Arc<str>>, i64) {
    let rows = world.rows().unwrap();
    let row = rows.agents.get(agent_id).unwrap();
    (row.activity, row.target_id.clone(), row.next_decision_at)
}

fn pending(world: &World) -> Vec<Arc<str>> {
    world.queue().unwrap().pending_ids(None)
}

fn counter(world: &World) -> i64 {
    world.id_counter("event").unwrap().unwrap()
}

// ---- wait, rest and a social action --------------------------------------------------------------

#[test]
fn wait_and_rest_take_their_activity_and_give_up_a_bed() {
    let world = world();
    let applied = proposals::apply(&world, &inputs(None), &proposal(Action::Wait, "a1")).unwrap();
    assert_eq!(applied, Applied { target: None, next_decision_at: 15, committed: true });
    assert_eq!(person(&world, "a1"), (Activity::Idle, None, 15));
    assert!(reservations(&world).is_empty());
    let applied = proposals::apply(&world, &inputs(None), &proposal(Action::Rest, "a2")).unwrap();
    assert_eq!(applied, Applied { target: None, next_decision_at: 15, committed: true });
    assert_eq!(person(&world, "a2").0, Activity::Resting);
    // No sequence was taken.
    assert_eq!(counter(&world), 10);
}

#[test]
fn a_social_action_schedules_its_interaction_with_someone_in_the_zone() {
    let world = world();
    let mut socialize = proposal(Action::Socialize, "a1");
    socialize.target_id = Some(text("a2"));
    let applied = proposals::apply(&world, &inputs(None), &socialize).unwrap();
    // The next decision waits for the interaction.
    assert_eq!(applied, Applied { target: Some(text("a2")), next_decision_at: 12, committed: true });
    assert_eq!(pending(&world), vec![text("autonomous-social-action-10")]);
    assert_eq!(counter(&world), 11);
    let queue = world.queue().unwrap();
    let (_, serial) = queue.ordered()[0];
    let event: &EventRow = queue.slot(serial).row.as_deref().unwrap();
    assert_eq!((event.kind, event.due_minute, event.priority, event.sequence), (EventKind::Endogenous, 12, 35, 10));
    assert_eq!(&*event.event_type, "social_interaction");
    let V::Map(payload) = &event.payload else { panic!("no payload") };
    let keys: Vec<&str> = payload.keys().map(|key| &**key).collect();
    assert_eq!(
        keys,
        [
            "interaction_type",
            "agent_id",
            "participant_agent_id",
            "conversation_state_context",
            "zone_id",
            "topic_id",
            "action",
            "reason_short",
            "interaction_duration_minutes"
        ]
    );
    assert_eq!(payload["interaction_type"], V::Str(text("reassurance_exchange")));
    assert_eq!(payload["interaction_duration_minutes"], V::Int(5));
    drop(queue);
    // Someone in another zone is not met: nothing is scheduled, and the decision does not commit.
    let mut apart = proposal(Action::HelpOthers, "a2");
    apart.target_id = Some(text("a3"));
    let applied = proposals::apply(&world, &inputs(None), &apart).unwrap();
    assert_eq!(applied, Applied { target: None, next_decision_at: 15, committed: false });
    assert_eq!(counter(&world), 11);
}

#[test]
fn a_resource_action_without_a_service_is_rejected() {
    let world = world();
    let applied = proposals::apply(&world, &inputs(None), &proposal(Action::FetchWater, "a2")).unwrap();
    assert_eq!(applied, Applied { target: None, next_decision_at: 15, committed: false });
    assert_eq!(metric(&world, "resource_service_request_rejected_count"), 1.0);
}

// ---- A request ----------------------------------------------------------------------------------

/// `seek_write` and then `seek_after` with the host.
fn ask<H: Host<Error = Error>>(cell: &Shard<World>, host: &mut H, inputs: &Inputs<'_>, proposal: &Proposal) -> Result<Applied, Error> {
    let sought = cell.with(|world| proposals::seek_write(world, inputs, proposal)).unwrap().map_err(|error| match error {
        sheltersim_core::events::writer::WriteError::Error(error) => error,
        other => panic!("{other:?}"),
    })?;
    proposals::seek_after(cell, host, inputs, proposal, sought)
}

#[test]
fn a_request_for_information_ties_the_heard_claims_to_it() {
    let cell = Shard::new(world());
    let mut host = TestHost::default();
    let inputs = inputs(Some(seek(Some(0.5))));
    let applied = ask(&cell, &mut host, &inputs, &proposal(Action::SeekInformation, "a1")).unwrap();
    let request_id = request_id_of("a1", 7);
    assert_eq!(applied, Applied { target: Some(request_id.clone()), next_decision_at: 15, committed: true });
    // A Run without an LLM asks nothing.
    assert!(host.calls.is_empty());
    let mut world = cell.try_borrow_mut().unwrap();
    assert_eq!(person(&world, "a1").0, Activity::SeekingHelp);
    assert_eq!(world.rows().unwrap().support_requests.get(&request_id).unwrap().status, RequestStatus::Pending);
    assert_eq!(pending(&world), vec![text("support-timeout-10")]);
    let state = world.row::<InformationRow>(INFORMATION).unwrap().clone();
    let [SupportRequestRow { request_id: recorded, agent_id, minute, message_ids }] = state.support_requests.as_slice() else {
        panic!("not one record");
    };
    assert_eq!((recorded, &**agent_id, *minute), (&request_id, "a1", 7));
    assert_eq!(message_ids, &vec![text("m1"), text("m2")]);
    assert_eq!(state.counters.support_requests, 1);
    assert_eq!(state.query_pressure.get("a1"), Some(&0.0));
    // a2 holds no heard claim: the record writes nothing, and leaves the namespace unmarked.
    world.slots.take_written();
    drop(world);
    ask(&cell, &mut host, &inputs, &proposal(Action::SeekInformation, "a2")).unwrap();
    let world = cell.try_borrow_mut().unwrap();
    assert!(!world.slots.written_names().iter().any(|name| &**name == INFORMATION));
    assert_eq!(world.row::<InformationRow>(INFORMATION).unwrap().counters.support_requests, 1);
    drop(world);
    // a3 has no beliefs: the Python rule's KeyError, after the request's writes.
    let refused = ask(&cell, &mut host, &inputs, &proposal(Action::SeekInformation, "a3"));
    assert_eq!(refused, Err(Error::Key("a3".to_owned())));
    let world = cell.try_borrow_mut().unwrap();
    assert_eq!(person(&world, "a3").0, Activity::SeekingHelp);
}

#[test]
fn seek_help_records_only_above_the_threshold() {
    let cell = Shard::new(world());
    let mut host = TestHost::default();
    // a2's pressure 0.25 is below 0.5: no record.
    ask(&cell, &mut host, &inputs(Some(seek(Some(0.5)))), &proposal(Action::SeekHelp, "a2")).unwrap();
    let world = cell.try_borrow_mut().unwrap();
    assert!(world.row::<InformationRow>(INFORMATION).unwrap().support_requests.is_empty());
    drop(world);
    ask(&cell, &mut host, &inputs(Some(seek(Some(0.5)))), &proposal(Action::SeekHelp, "a1")).unwrap();
    let world = cell.try_borrow_mut().unwrap();
    assert_eq!(world.row::<InformationRow>(INFORMATION).unwrap().support_requests.len(), 1);
}

#[test]
fn what_a_request_refuses_it_refuses_before_any_write() {
    let world = world();
    let refused = |inputs: &Inputs<'_>, proposal: &Proposal| {
        let mut copy = World::fork(&world).unwrap();
        let error = match proposals::seek_write(&mut copy, inputs, proposal) {
            Err(sheltersim_core::events::writer::WriteError::Error(error)) => error,
            other => panic!("{:?}", other.map(|_| ())),
        };
        assert_eq!(counter(&copy), 10);
        assert_eq!(person(&copy, "a1"), (Activity::Idle, None, 0));
        assert_eq!(reservations(&copy), vec![text("a1")]);
        error
    };
    assert_eq!(refused(&inputs(Some(seek(Some(0.5)))), &proposal(Action::SeekHelp, "nobody")), unsupported("no such agent"));
    // The threshold whose read raised is read where the rule reads it.
    assert_eq!(refused(&inputs(Some(seek(None))), &proposal(Action::SeekHelp, "a1")), unsupported(THRESHOLD_UNREAD));
    // A seek_information reads no threshold.
    let mut copy = World::fork(&world).unwrap();
    assert!(proposals::seek_write(&mut copy, &inputs(Some(seek(None))), &proposal(Action::SeekInformation, "a1")).is_ok());
    // A shared operations namespace that is None.
    let none = Seek { shared: Held::Other(NAMESPACE_IS_NONE), ..seek(Some(0.5)) };
    assert_eq!(refused(&inputs(Some(none)), &proposal(Action::SeekHelp, "a1")), unsupported(NAMESPACE_IS_NONE));
    // A staff service whose value raised.
    let unread = Seek { service: RequestService { unresolved_after_minutes: None, ..service() }, ..seek(Some(0.5)) };
    assert_eq!(
        refused(&inputs(Some(unread)), &proposal(Action::SeekHelp, "a1")),
        unsupported(sheltersim_core::rules::support::AFTER_UNREAD)
    );
}

// ---- The LLM's hooks, through a host that writes the world (rule R6) ------------------------------

/// What the host does when asked for the support priority.
#[derive(Clone, Copy, PartialEq)]
enum Priority {
    /// Writes the request's status, schedules an event of its own and moves the person's next
    /// decision, as the Python hooks may.
    Writes,
    /// Counts a refusal itself and raises a `ValueError`.
    Raises,
}

/// A host whose LLM hooks write the world they are given: the kernel holds no borrow of it while
/// they run, and reads again after them what it returns.
struct WritingHost<'a> {
    cell: &'a Shard<World>,
    inner: TestHost,
    priority: Priority,
}

impl Host for WritingHost<'_> {
    type Error = Error;

    fn validate_candidate(&mut self, kind: TxnKind, stage: Stage) -> Result<(), Error> {
        self.inner.validate_candidate(kind, stage)
    }
    fn canonicalized(&mut self, kind: TxnKind, before: &Snapshot) -> Result<(), Error> {
        self.inner.canonicalized(kind, before)
    }
    fn validate_world(&mut self) -> Result<(), Error> {
        self.inner.validate_world()
    }
    fn validate_extensions(&mut self) -> Result<(), Error> {
        self.inner.validate_extensions()
    }
    fn contract(&mut self, which: &Contract, input: &V) -> Result<V, Error> {
        self.inner.contract(which, input)
    }
    fn llm(&self) -> Result<Llm, Error> {
        Ok(Llm { router: true, social_policy: true })
    }
    fn prioritize_support_request(&mut self, request_id: &str, agent_id: &str, hash: &str) -> Result<(), Error> {
        self.inner.prioritize_support_request(request_id, agent_id, hash)?;
        // The kernel lent nothing: the host borrows the world itself.
        let world = self.cell.try_borrow_mut().expect("the kernel holds no borrow of the world");
        match self.priority {
            Priority::Writes => {
                world.rows_mut()?.support_requests.get_mut(request_id).unwrap().status = RequestStatus::Assigned;
                world.rows_mut()?.agents.get_mut(agent_id).unwrap().next_decision_at = 42;
                let events = Events::of(&world)?;
                let sequence = events.allocate_sequence()?;
                let event = EventRow::new(
                    text(&format!("host-event-{sequence}")),
                    EventKind::Endogenous,
                    20,
                    10,
                    sequence,
                    text("llm_note"),
                    IndexMap::new(),
                    None,
                    None,
                    IndexMap::new(),
                )
                .unwrap();
                events.schedule(event).map_err(|_| Error::Runtime("not scheduled".to_owned()))?;
                Ok(())
            }
            Priority::Raises => {
                sheltersim_core::rules::kit::add_metric(&world, "support_request_rejected_count")?;
                Err(Error::Value("the Provider's answer is not JSON".to_owned()))
            }
        }
    }
    fn consider_support_request(&mut self, agent_id: &str, request_id: &str) -> Result<(), Error> {
        assert!(self.cell.try_borrow_mut().is_ok(), "the kernel holds no borrow of the world");
        self.inner.consider_support_request(agent_id, request_id)
    }
    fn consider_complaint_response(&mut self, agent_id: &str, request_id: &str) -> Result<(), Error> {
        self.inner.consider_complaint_response(agent_id, request_id)
    }
    fn consider_cooperation(&mut self, initiator: &str, participant: &str, trigger: &str) -> Result<(), Error> {
        self.inner.consider_cooperation(initiator, participant, trigger)
    }
    fn consider_information_reaction(&mut self, agent_id: &str, participant: &str, message: &str, trigger: &str) -> Result<(), Error> {
        self.inner.consider_information_reaction(agent_id, participant, message, trigger)
    }
    fn capture_metric(&mut self, sample: &MetricSample, minute: i64) -> Result<(), Error> {
        self.inner.capture_metric(sample, minute)
    }
    fn decision_sink(&self) -> Result<DecisionSink, Error> {
        self.inner.decision_sink()
    }
    fn decision(&mut self, decision: Decision) -> Result<(), Error> {
        self.inner.decision(decision)
    }
    fn trace_level(&self) -> Result<TraceLevel, Error> {
        self.inner.trace_level()
    }
    fn trace(&mut self, document: Trace) -> Result<(), Error> {
        self.inner.trace(document)
    }
    fn phase(&mut self, phase: Phase, before: bool) -> Result<(), Error> {
        self.inner.phase(phase, before)
    }
    fn space_refreshed(&mut self) -> Result<(), Error> {
        self.inner.space_refreshed()
    }
}

#[test]
fn the_hooks_run_with_nothing_borrowed_and_the_step_reads_again_after_them() {
    let cell = Shard::new(world());
    let mut host = WritingHost { cell: &cell, inner: TestHost::default(), priority: Priority::Writes };
    let inputs = inputs(Some(seek(Some(0.5))));
    let applied = ask(&cell, &mut host, &inputs, &proposal(Action::SeekHelp, "a1")).unwrap();
    let request_id = request_id_of("a1", 7);
    // The next decision is the one the host wrote, read after it.
    assert_eq!(applied, Applied { target: Some(request_id.clone()), next_decision_at: 42, committed: true });
    // In this order, each once: the priority, then the social policy.
    assert_eq!(
        host.inner.calls,
        vec![
            Call::Prioritize { request: request_id.clone(), agent: text("a1"), hash: text(&"a".repeat(64)) },
            Call::SupportRequest { agent: text("a1"), request: request_id.clone() },
        ]
    );
    let world = cell.try_borrow_mut().unwrap();
    // The host's writes stand: the kernel wrote nothing over them.
    assert_eq!(world.rows().unwrap().support_requests.get(&request_id).unwrap().status, RequestStatus::Assigned);
    // The host's event took the sequence after the request's timeout (the queue is in due order).
    assert_eq!(pending(&world), vec![text("host-event-11"), text("support-timeout-10")]);
    assert_eq!(counter(&world), 12);
    assert_eq!(metric(&world, "support_request_rejected_count"), 0.0);
}

#[test]
fn a_value_error_of_a_hook_is_a_refused_request_counted_on_what_the_host_wrote() {
    let cell = Shard::new(world());
    let mut host = WritingHost { cell: &cell, inner: TestHost::default(), priority: Priority::Raises };
    let inputs = inputs(Some(seek(Some(0.5))));
    let applied = ask(&cell, &mut host, &inputs, &proposal(Action::SeekHelp, "a1")).unwrap();
    assert_eq!(applied, Applied { target: Some(request_id_of("a1", 7)), next_decision_at: 15, committed: true });
    // `except ValueError`: the social policy is not asked, and the count goes on from the host's.
    assert_eq!(host.inner.calls.len(), 1);
    let world = cell.try_borrow_mut().unwrap();
    assert_eq!(metric(&world, "support_request_rejected_count"), 2.0);
}

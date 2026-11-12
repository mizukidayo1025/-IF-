"""Application handlers and deterministic dispatch for due events."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Callable, Protocol

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.world import WorldState
from sheltersim.domain.facilities import ToiletServiceManager
from sheltersim.domain.resource_service import ResourceServiceManager
from sheltersim.events.effects import (
    EffectProgramEnvironmentHandler,
    EffectProgramFacilityHandler,
    EffectProgramGeometryHandler,
    EffectProgramStructureHandler,
    EffectProgramTemporarySpaceHandler,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventLogRecord,
    EventStatus,
    ScheduledEvent,
)

if TYPE_CHECKING:
    from sheltersim.core.engine import TickPhase


class EventHandler(Protocol):
    """Apply one event atomically, raising expected outcomes before mutation."""

    def apply(
        self, world: WorldState, event: ScheduledEvent
    ) -> tuple[EventLogRecord, ...] | None: ...


class EventDispatcher:
    """Dispatch each due event once during the engine's first tick phase."""

    def __init__(self, handlers: dict[str, EventHandler] | None = None) -> None:
        self.handlers: dict[str, EventHandler] = dict(handlers or {})

    def register(self, event_type: str, handler: EventHandler) -> None:
        """Register or replace the handler for a stable event type."""
        if not event_type or event_type in self.handlers:
            raise ValueError(f"event handler already registered or invalid: {event_type!r}")
        self.handlers[event_type] = handler

    def __call__(self, phase: TickPhase, world: WorldState, minute: int, rng: object) -> None:
        """Use as a TickEngine phase handler; other phases remain untouched."""
        if phase == "dispatch_due_events":
            self.dispatch_due(world, minute)

    def dispatch_due(
        self,
        world: WorldState,
        minute: int,
        *,
        after_event: Callable[[tuple[EventLogRecord, ...]], None] | None = None,
    ) -> tuple[EventLogRecord, ...]:
        """Apply due events in order; observe each committed outcome before the next."""
        outcomes: list[EventLogRecord] = []
        for event in world.event_queue.due(minute):
            outcomes.extend(self.dispatch_one(world, event, minute, after_event=after_event))
        return tuple(outcomes)

    def dispatch_one(
        self,
        world: WorldState,
        event: ScheduledEvent,
        minute: int,
        *,
        after_event: Callable[[tuple[EventLogRecord, ...]], None] | None = None,
    ) -> list[EventLogRecord]:
        """Apply one event of a due snapshot; its outcomes, or none if it left the queue meanwhile.

        The core does it (`_dispatch_one`); then `after_event` sees the outcomes.
        """
        outcomes = self._dispatch_one(world, event, minute)
        if outcomes and after_event is not None:
            after_event(tuple(outcomes))
        return outcomes

    @kernel_only("dispatch", "dispatch_one")
    def _dispatch_one(
        self, world: WorldState, event: ScheduledEvent, minute: int
    ) -> list[EventLogRecord]:
        """`dispatch_one` without `after_event`: native/core/src/rules/dispatch.rs `one_alone`
        (decision D3 of Issue #152, lane P9-3b).

        Nothing when the queue no longer holds the event, or holds another of its ID whose
        `to_dict()` differs (a preceding event of the same batch cancelled or replaced it: the
        cancellation emitted its own record). Else the handler `self.handlers.get(event_type)`:
        none is the FAILED outcome "no handler registered for event type 'T'". The EffectProgram's
        guard runs first (`should_apply_effect_guard`; false is EventSuppressed "EffectProgram guard
        evaluated false"); then `handler.apply(world, event)`.
        Its EventSuppressed or EventApplicationError is the SUPPRESSED or FAILED outcome with
        `str(exc)`; anything else it raises goes on. An applied information_contact_update,
        social_interaction, information_correction or proximity_contact_update is recorded
        (`record_social_event`, and `observe_event` with social_network_v1 in `model_state`).
        Then `settle`, and the records `apply` returned: each one the log holds must be the same
        event with the same status (ValueError "transactional follow-up record disagrees with
        dispatch result"), and any other is appended. Returns the log's records of the event and
        of those, in that order. An event no row can hold is a KernelRefusal.
        """

    @kernel_only("dispatch", "settle")
    def settle(
        self,
        world: WorldState,
        event: ScheduledEvent,
        minute: int,
        status: EventStatus,
        detail: str | None,
    ) -> EventLogRecord:
        """Take a handled event off the queue and record its outcome, unless its handler did.

        native/core/src/rules/dispatch.rs `settle_alone` (decision D3, lane P9-3b). A record the
        log holds of its ID must be of the same event and status (ValueError "transactional event
        record disagrees with its dispatch result"). Returns the log's record.
        """


@dataclass(frozen=True)
class ProximityContactHandler:
    """Accept a sampled physical encounter without asserting a conversation."""

    @kernel_only("handlers", "proximity")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Check a sampled encounter against the current positions; it changes nothing.

        EventApplicationError, in this order, when the social_proximity config is absent or the
        event is not the endogenous ASM-PROXIMITY-CONTACT-V1 sample of this minute with its four
        provenance fields; when the payload fields differ; when the two Agents, the zone, the
        observed minute, `contact_minutes` (0) or the distance (finite, 0 to max_distance_m) are
        invalid; when a point is not finite {x, y}; when the distance is not the points' hypot.
        EventSuppressed ("proximity contact sample is stale") when either Agent's physical position
        is no longer that point in that zone.
        """


@dataclass(frozen=True)
class InformationRumorHandler:
    """Seed one local Agent with a typed, provenance-linked claim."""

    @kernel_only("information_events", "rumor")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...] | None:
        """Seed the origin Agent with a typed claim: the message, a "heard" belief, query pressure.

        The information-spread state is upgraded from schema 1 first, as
        `upgrade_information_runtime_state` does. EventApplicationError when the model is off or its
        config or state is invalid, and for the payload fields, a missing source_id, IDs that are
        not str, a truth status that is not one, a message ID that exists, 1024 messages, or an
        unknown Agent. The belief strength is 0.5, or under evidence-aware-rumor-v1 a CounterRNG
        draw keyed ("rumor-belief-acceptance-v1", minute, agent, message).
        """


@dataclass(frozen=True)
class InformationCorrectionHandler:
    """Publish a zone-scoped, source-linked correction to one existing claim."""

    @kernel_only("information_events", "correction")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Publish a verified or rejected correction to everyone in the zone.

        The information-spread state is upgraded from schema 1 first. EventApplicationError when the
        model is off, and for the payload fields, a missing source_id, IDs that are not str, a
        belief that is not one or neither verifies nor rejects, an unknown zone or message, a
        causation that is not the rumor, a belief against the message's truth, or an Agent already
        holding the other correction ("conflicting verified information state"). An unknown truth
        status takes the correction's. Under evidence-aware-rumor-v1 each Agent accepts by a
        CounterRNG draw keyed ("correction-acceptance-v1", minute, source, agent, message); an
        accepted correction relieves query pressure and is counted.
        """


@dataclass(frozen=True)
class InformationContactHandler:
    """Apply one sampled, still-local contact at the next tick boundary."""

    @kernel_only("information_events", "contact")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Apply a sampled contact that still holds: the recipient's belief, pressure, counters.

        The information-spread state is upgraded from schema 1 first. EventApplicationError when the
        model is off, and for the payload fields, a kind other than endogenous, another minute, the
        provenance, the IDs, one Agent twice, unknown state, an invalid belief, a duration below the
        configured minimum, an invalid probability, a source or causation that is not the sender's,
        or a correction against the message's truth. EventSuppressed when the sender no longer knows
        the message or holds another belief, when the two are no longer in the same, an adjacent or
        a family contact (or not that basis), or when the recipient's belief is as strong (unless a
        correction under evidence-aware-rumor-v1). Draws under evidence-aware-rumor-v1, keyed
        without the event's ID: ("contact-correction-acceptance-v1", minute, sender, recipient,
        message) for a correction and ("rumor-belief-acceptance-v1", minute, recipient, message)
        for a heard claim.
        """


@dataclass(frozen=True)
class ResourceLedgerHandler:
    """Apply one versioned inventory operation after full validation."""

    operation: str

    @kernel_only("handlers", "resource_ledger")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Apply one inventory operation (`self.operation`) to the resource ledger.

        A delivery with a shipment_id must match its operational shipment record (else
        EventApplicationError), and the ledger sees it without that field. On WorldState v6 with a
        resource service, a release or consume of a service-owned reservation, or a reserve whose ID
        is a queued service task, is an EventApplicationError. `apply_resource_operation` does the
        rest; its ResourceLedgerError is the EventApplicationError's text.
        """


@dataclass(frozen=True)
class FacilityOutageHandler:
    """Reduce a facility's working capacity after validating the full change."""

    @kernel_only("handlers", "facility_outage")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...] | None:
        """Take `units_out` working units out of a facility, and record the change.

        EventApplicationError, in this order: payload fields other than facility_id and units_out, a
        facility_id that is not a non-empty str, units that are not an int or are negative, an
        unknown facility, an active infrastructure outage or EffectProgram availability modifier on
        it, more units than are working. EventSuppressed for zero units. People being served finish
        (#188). The infrastructure state's base units follow. Returns the facility recovery receipt,
        if any.
        """


@dataclass(frozen=True)
class FacilityRecoveryHandler:
    """Restore operational units without exceeding installed capacity."""

    @kernel_only("handlers", "facility_recovery")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...] | None:
        """Restore `units_restored` working units of a facility, up to its capacity, and record it.

        EventApplicationError, in this order: payload fields other than facility_id and
        units_restored, a facility_id that is not a non-empty str, units that are not an int or are
        negative, an unknown facility, an active infrastructure outage or EffectProgram availability
        modifier on it, more units than the capacity holds. EventSuppressed for zero units. The
        infrastructure state's base units follow. Returns the facility recovery receipt, if any.
        """


class InfrastructureEventHandler:
    """Apply versioned outages, time-bounded environment stress, and route disruptions."""

    @kernel_only("handlers", "infrastructure")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...] | None:
        """Apply an infrastructure event on a copy of the world, committed only once it validates.

        power_outage and equipment_failure start their scenario outage and schedule its restoration;
        power_restored and equipment_repaired end it; working units are then recalculated from every
        active cause. extreme_heat and extreme_cold start a zone stressor (16 per zone) and schedule
        its end; environmental_stressor_end ends it. supply_route_interruption delays its pending
        shipments by the interruption (each a recorded delivery_delayed) and schedules the
        restoration; supply_route_restored ends it. Each mismatch with the infrastructure state is
        an EventApplicationError with its own text, as is another event type. Returns the delays'
        records and a facility recovery receipt for each facility whose working units changed.
        """


@dataclass(frozen=True)
class SocialInteractionHandler:
    """Apply one allowlisted, bounded social proposal at its scheduled minute."""

    @kernel_only("social_interaction", "apply")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Apply one social interaction to the people it names.

        Without an interaction_type: agent_id, request_id and action must be non-empty str
        (EventApplicationError "social interaction requires Agent, request, and action IDs") and
        the request must be the known Agent's ("social interaction references an unknown request or
        Agent"). offer_reassurance takes 0.03 off the Agent's stress and adds 0.01 trust,
        share_practical_information adds 0.005 trust; another action is not allowed.

        A typed interaction is an endogenous event due now with the provenance of
        ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1 (its source_id the policy_version), a reason code
        and a known Agent; a conversation state context must pass `validate_social_state_context`
        ("social dialogue state context is invalid"). Then each type checks its payload fields,
        values, action and reason code (EventApplicationError) and whether the situation still holds
        (EventSuppressed), in the rule's order:

        - reassurance_exchange, family_care and voluntary_support need the participant in the
          Agent's zone (family care: of the same household; voluntary support: a highest need of
          stress, toilet urgency, hunger, fatigue and hydration above the helper's by more than
          0.15). The participant is relieved by `social_action_affect_transfer` (0.02) times the
          Agent's positive minus negative affect (body state, 0.5 and 0.0 by default) clipped to
          -1..1: stress down by it, trust up by half of it, both within 0..1. Except in a
          reassurance exchange, the Agent gains 0.002 fatigue and a quarter of a positive relief as
          trust.
        - complaint_response needs the Agent's pending request, waited for at least
          complaint_wait_minutes, and stress at the threshold: acknowledge_complaint (stress -0.02,
          trust +0.01), explain_staff_process (stress -0.01, trust +0.01) or
          share_practical_information (trust +0.005).
        - cooperation needs two people of one household in one zone, after the Agent's applied
          contact with the participant the minute before, and either at the stress threshold:
          offer_mutual_support takes 0.02 off both stresses and adds 0.01 to both trusts.
        - information_reaction needs the participant's applied contact with the Agent the minute
          before and a false claim the Agent has only heard: seek_claim_verification raises the
          Agent's query pressure by `query_pressure_per_claim`, up to 1.

        Another type is "typed social interaction type is not allowed".
        """


@dataclass(frozen=True)
class EffectProgramPopulationHandler:
    """Apply typed population primitives through the approved EffectProgram record."""

    @kernel_only("handlers", "population")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Apply an approved population primitive: arrivals, departures or a presence command.

        population_approach_complete is `apply_arrival_approach_completion`. Otherwise the op is
        add_evacuees, depart_evacuees or population_presence (EventApplicationError "population
        primitive operation is invalid"), with its payload fields, older add_evacuees and
        depart_evacuees payloads included ("population primitive payload fields do not match"), and
        the event is a scheduled event of its approved program, of that operator, for a known zone
        ("population primitive is not in the approved program"). population_presence is
        `apply_population_presence_command`. add_evacuees is `queue_arrivals`, depart_evacuees
        `queue_departures` and then `admit_pending_returns`, both on a copy of the world that takes
        its place once it validates, also when they raise EventSuppressed; their TypeError or
        ValueError is an EventApplicationError with its text.
        """


@dataclass(frozen=True)
class ToiletServiceCompletionHandler:
    """Apply one due service completion, releasing its reserved toilet slot."""

    after_complete: Callable[[WorldState, ScheduledEvent], None] | None = None

    @kernel_only("toilet", "complete")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Complete a due toilet service: release its slot, then `self.after_complete`, if set.

        EventApplicationError when facility_id, agent_id or task_id is not a non-empty str, when the
        task is not active and due now, and when the facility is not a toilet, the person is
        unknown, the task is not this person's active toilet service there, the person is not in
        service there or doesn't hold exactly one slot. The queue's record finishes, the slot and a
        reserved staff member are freed, the person is idle with no toilet urgency, and a completed
        visit takes 0.2 off the person's hygiene need (not below 0).
        """


@dataclass(frozen=True)
class AgentTravelCompletionHandler:
    """Complete a scheduled walk and optionally register its toilet request."""

    toilet_service: ToiletServiceManager
    resource_service: ResourceServiceManager | None = None
    validate_request_state: bool = True

    @kernel_only("travel_completion", "apply")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Complete a walk: the person reaches its destination, then `_after_arrival`.

        EventApplicationError when agent_id, task_id, destination_zone_id or purpose is not a
        non-empty str, when the task is not an active walk or queue task, and when it is not the
        person's. A walk to a queue's place or service point (purpose toilet_ or
        resource_queue_position or _service) changes nothing: it must fit its FIFO reservation, the
        queue task, the queued person at the facility's slot or service point and its journey
        ("physical queue travel does not match its FIFO reservation"). Any other walk must be due
        now, with the person moving to a known zone, and fit its purpose: toilet_request,
        resource_request (with a resource service and a route from its origin), return and
        queue_abandon, staff_support_travel (the assigned request and its staff member there),
        sleep_bed (the live bed reservation and its journey) or presence_relocation (the pending
        relocation); another is "unsupported travel purpose ...". Each with its own text, checked
        before any change.

        The task ends and the person is in the destination zone with no target: seeking help after
        a support walk, asleep at the reserved bed after a sleep_bed walk (#84), else idle. A
        relocation is completed by `complete_presence_relocation`. The origin given to
        `_after_arrival` is the event's origin_zone_id when a str, else the first of its
        route_zone_ids, else None.
        """

    @kernel_only("travel_completion", "after_arrival")
    def _after_arrival(
        self,
        world: WorldState,
        event: ScheduledEvent,
        agent_id: str,
        purpose: str,
        facility_id: Any,
        destination_zone_id: str,
        origin_zone_id: Any,
    ) -> None:
        """What a walk to a toilet or a resource leads to; nothing for another purpose.

        With a floor plan and a facility without independent queues, the person first walks in the
        room to the facility (`facility_approach_route`): a walk of its minutes is scheduled
        (agent-facility-approach-v2), or, with no route or none that ends within the Run,
        `_reject_room_approach`. Then a toilet walk asks `toilet_service.request` (which validates
        the world when `validate_request_state` is set); its ValueError is an EventApplicationError
        with its text. A resource walk asks `resource_service.request`; its ValueError leaves the
        person idle at the facility, and the outcome is recorded as rejected ("request_rejected",
        the text as its detail, fetch_water at a water facility, else collect_meal) and counted in
        resource_service_request_rejected_count.
        """

    @kernel_only("travel_completion", "reject_room_approach")
    def _reject_room_approach(
        self,
        world: WorldState,
        event: ScheduledEvent,
        agent_id: str,
        facility_id: str,
        origin_zone_id: Any,
    ) -> None:
        """Count a room approach that was turned away.

        For a resource request with a resource service, the request's outcome is recorded as
        rejected ("room_route_blocked"; fetch_water at a water facility, else collect_meal), from
        the event's origin zone when it is a str, else the person's zone, and counted. Always
        counted as facility_approach_rejected_count.
        """


@dataclass(frozen=True)
class ResourceServiceCompletionHandler:
    """Apply a scheduled service completion through the resource service state machine."""

    service: ResourceServiceManager

    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        if world.schema_version != 6:
            raise EventApplicationError("resource service completion requires WorldState v6")
        self.service.complete(event)


@dataclass(frozen=True)
class ResourceServiceCancellationHandler:
    """Apply an explicit typed cancellation with one-time reservation release."""

    service: ResourceServiceManager

    @kernel_only("handlers", "service_cancellation")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """`self.service.cancel(event)`; EventApplicationError before WorldState v6."""


@dataclass(frozen=True)
class DeliveryDelayHandler:
    """Reschedule one pending shipment without changing its stable identity."""

    @kernel_only("handlers", "delivery_delay")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Move a pending shipment's delivery to `to_minute`, on a copy of the world.

        EventApplicationError when the payload is not shipment_id and to_minute or there is no
        source, without operational_events_v1, for an unknown shipment, when its delivery is no
        longer pending as recorded, and unless the delivery is not yet due and to_minute is a later
        int before the Run's end. The delivery keeps its identity, notes the delay event and takes a
        derived EffectProgram marker.
        """


@dataclass(frozen=True)
class WaterOutageHandler:
    """Block new staffed water service at an explicitly named target."""

    @kernel_only("handlers", "water_outage")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Start a recorded water outage of a zone's or a facility's water station, on a copy.

        EventApplicationError for the payload fields or a missing source, an outage that is not this
        event's in the operational state, another target, a zone without a water_station or a
        facility that isn't one. EventSuppressed when it is already active.
        """


@dataclass(frozen=True)
class WaterRestorationHandler:
    """Restore new water service starts for one correlated outage."""

    @kernel_only("handlers", "water_restoration")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """End a recorded water outage, on a copy.

        EventApplicationError for the payload fields, a missing source, or an outage whose
        restoration is not this event. EventSuppressed when it is not active.
        """


@dataclass(frozen=True)
class StaffUnavailabilityHandler:
    """Remove one staff member from assignment and safely interrupt active work."""

    @kernel_only("handlers", "staff_unavailable")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...]:
        """Take one staff member out of assignment until `until_minute`, on a copy of the world.

        EventApplicationError for the payload fields or a missing source, without operational event
        state, for unknown staff or an until_minute that is not an int after now within the Run, and
        when the staff member holds more than one of a support task, a resource service task and an
        autonomous operation. EventSuppressed when this absence was already applied. Schedules
        staff_restored (with a derived EffectProgram marker) unless the absence lasts to the end. A
        resource service is interrupted by its manager, an operation by `interrupt_staff_operation`,
        and a support task is requeued or left unresolved (its resident's trip suppressed, a timeout
        rescheduled), with its continuity record: the records returned.
        """


@dataclass(frozen=True)
class StaffRestorationHandler:
    """Restore staff only when the event still matches the latest absence token."""

    @kernel_only("handlers", "staff_restoration")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Make a staff member available again if this still matches the latest absence, on a copy.

        EventApplicationError for the payload fields, a missing source, unknown staff, or a token
        that is not this event's. EventSuppressed when it belongs to an older absence or the staff
        member is available.
        """


def _ensure_event_identity_free(world: WorldState, event: ScheduledEvent) -> None:
    if (
        any(
            current.event_id == event.event_id or current.sequence == event.sequence
            for current in world.event_queue.pending()
        )
        or world.event_log.contains_id(event.event_id)
        or world.event_log.contains_sequence(event.sequence)
    ):
        raise EventApplicationError(f"generated event identity {event.event_id!r} is not unique")


@dataclass(frozen=True)
class StaffSupportCompletionHandler:
    """Finish a staff support task and start the Agent cooldown."""

    cooldown_minutes: int = 30
    require_colocated: bool = False

    @kernel_only("support", "complete_support")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...] | None:
        """Finish a due staff support task with `complete_support_task`, then what the care did.

        EventApplicationError when request_id, agent_id, staff_id or task_id is not a non-empty str
        and when the task is not active and due now; EventSuppressed when it is a cancelled staff
        task. A ValueError of `complete_support_task` is an EventApplicationError with its text.
        Without a care record of the request, nothing more: None.

        Only with the person's record in the body state: emotional support relieves the person by
        `complaint_reassurance_gain` (its value by Agent first), stress down by it, trust up by a
        quarter of it, reassurance up by it, positive affect up and negative affect down by half of
        it; hygiene support takes `hygiene_support_relief` off the hygiene need, through the
        selected care outcome formula when there is one. Another kind changes what the formula's
        deltas say, and caregiver assistance also those of the person's explicit care dependents.
        Values stay within 0..1. Returns the completed care transition record, with each measured
        value before and after (observed_change, observed_no_change, or not_measured without the
        body record) and its sources.
        """


@dataclass(frozen=True)
class StaffOperationArrivalHandler:
    """Move a staff member to the assigned task after its actual route time."""

    @kernel_only("staff_operation_events", "arrive")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """The travelling staff member reaches the task: the operation's phase becomes working.

        EventApplicationError for another event type, and when the event doesn't match the staff
        member's travelling operation (task, staff, due minute, target zone). When the staff member
        is no longer available, `interrupt_staff_operation` stops the task and the event is
        EventSuppressed ("staff became unavailable before reaching the assigned task").
        """


@dataclass(frozen=True)
class StaffOperationReliefHandler:
    """Transfer an in-flight staff operation at an applied relief boundary."""

    @kernel_only("staff_operation_events", "relief")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...] | None:
        """An in-flight operation goes to a qualified co-located relief; the handoff receipt.

        EventApplicationError for a relief whose provenance or payload is not the relief policy's,
        and when the task has no pending completion. EventSuppressed when the relief is stale or the
        replacement is no longer free and available, or lacks the task's skill. The task's pending
        arrival and completion are recorded as suppressed, the replacement works the task from now
        for its configured minutes, the previous member is idle, and a new completion is scheduled.
        """


@dataclass(frozen=True)
class StaffOperationCompletionHandler:
    """Apply routed cleaning, repair, intake, outreach or patrol work."""

    @kernel_only("staff_operation_events", "complete")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...] | None:
        """A staff operation's work is done: its effect, its history row and its receipt.

        EventApplicationError for another event type, and when the event doesn't match the staff
        member's working operation. When the staff member is no longer available,
        `interrupt_staff_operation` stops the task and the event is EventSuppressed ("staff became
        unavailable before completing the assigned task"). Cleaning raises the zone's sanitation,
        maintenance restores one working unit (blocked at capacity or under an outage or modifier),
        outreach and patrol relieve the zone's people, intake admits pending arrivals. The staff
        member is idle again. The receipt lists the numeric fields the task changed: a cleaned
        zone's sanitation, a maintained facility's working units, and in an outreach or patrol zone
        each person's trust, uncertainty, reassurance and positive affect. A maintenance that
        changed working units also gives a facility recovery receipt.
        """


@dataclass(frozen=True)
class SupportRequestTimeoutHandler:
    """Close a pending request at its deadline; assigned work is not interrupted."""

    cooldown_minutes: int = 30

    @kernel_only("support", "timeout")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        """Close a pending support request at its deadline; assigned work goes on.

        EventApplicationError when request_id or agent_id is not a str, and when the request is not
        the Agent's. EventSuppressed when it is no longer pending. Otherwise it is unresolved, with
        a cooldown of `self.cooldown_minutes`, and the person is idle.
        """


# The event types with a Rust function in rules/dispatch.rs. A patch of `apply` shows here, and the
# handler then goes to Python. The EffectProgram handlers are there for an event whose guard held:
# the loop asks the guard as `dispatch_one` does, then calls `apply`.
native.rules("dispatch").install_table(
    (SocialInteractionHandler, SocialInteractionHandler.apply),
    (AgentTravelCompletionHandler, AgentTravelCompletionHandler.apply),
    (ToiletServiceCompletionHandler, ToiletServiceCompletionHandler.apply),
    (ResourceServiceCompletionHandler, ResourceServiceCompletionHandler.apply),
    (SupportRequestTimeoutHandler, SupportRequestTimeoutHandler.apply),
    (StaffSupportCompletionHandler, StaffSupportCompletionHandler.apply),
    [
        (handler, handler.apply)
        for handler in (
            ProximityContactHandler,
            InformationRumorHandler,
            InformationCorrectionHandler,
            InformationContactHandler,
            ResourceLedgerHandler,
            FacilityOutageHandler,
            FacilityRecoveryHandler,
            InfrastructureEventHandler,
            ResourceServiceCancellationHandler,
            EffectProgramPopulationHandler,
            DeliveryDelayHandler,
            WaterOutageHandler,
            WaterRestorationHandler,
            StaffUnavailabilityHandler,
            StaffRestorationHandler,
            StaffOperationArrivalHandler,
            StaffOperationReliefHandler,
            StaffOperationCompletionHandler,
            EffectProgramFacilityHandler,
            EffectProgramEnvironmentHandler,
            EffectProgramTemporarySpaceHandler,
            EffectProgramGeometryHandler,
            EffectProgramStructureHandler,
        )
    ],
    AgentTravelCompletionHandler._after_arrival,
    EventDispatcher,
)

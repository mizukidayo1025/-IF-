"""Application handlers and deterministic dispatch for due events."""

from __future__ import annotations

from collections.abc import Mapping
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any, Callable, Protocol

from sheltersim import native
from sheltersim.contracts.operations import NumericStateChange
from sheltersim.core.kernels import kernel_first, kernel_only
from sheltersim.core.world import WorldState
from sheltersim.domain.care_receipts import care_receipt_for_request, make_care_transition_record
from sheltersim.domain.facilities import ToiletServiceManager
from sheltersim.domain.information import INFORMATION_SPREAD_STATE_KEY
from sheltersim.domain.models import (
    ActiveTask,
    Activity,
    AgentInformationState,
    InformationBeliefStatus,
    InformationSpreadConfig,
    InformationTruthStatus,
    SupportRequestStatus,
    TypedMessage,
)
from sheltersim.domain.movement import (
    MOVEMENT_STATE_KEY,
    agent_is_at_facility_service_point,
    agent_is_at_queue_slot,
    agent_is_at_queue_trip_destination,
    facility_approach_route,
    start_movement,
)
from sheltersim.domain.operational import OPERATIONAL_EVENTS_KEY
from sheltersim.domain.resource_service import ResourceServiceManager
from sheltersim.domain.social_dialogue import validate_social_state_context
from sheltersim.domain.staff import complete_support_task
from sheltersim.events.effects import (
    EffectProgramEnvironmentHandler,
    EffectProgramFacilityHandler,
    EffectProgramGeometryHandler,
    EffectProgramStructureHandler,
    EffectProgramTemporarySpaceHandler,
    effect_event_guard_or_suppress,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventLogRecord,
    EventStatus,
    EventSuppressed,
    ScheduledEvent,
)

if TYPE_CHECKING:
    from sheltersim.core.engine import TickPhase


class EventHandler(Protocol):
    """Apply one event atomically, raising expected outcomes before mutation."""

    def apply(
        self, world: WorldState, event: ScheduledEvent
    ) -> tuple[EventLogRecord, ...] | None: ...


def _current_household_id(world: WorldState, agent_id: str) -> str | None:
    """Use mutable presence household assignments while retaining profile fallback."""
    from sheltersim.domain.population_lifecycle import current_household_id

    return current_household_id(world, agent_id)


def _social_payload_fields(payload: Mapping[str, Any]) -> set[str]:
    return set(payload) - {"conversation_state_context"}


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

        native/src/rules/dispatch.rs calls this for each event it has no Rust function for.
        """
        current = world.event_queue.find(event.event_id)
        if current is None:
            # A preceding event in this same due batch may cancel this
            # snapshot entry (departure, shift interruption, or replacement).
            # The cancellation path already emitted the single audit record.
            return []
        if current is not event and current.to_dict() != event.to_dict():
            # A preceding due event may have replaced this item with a new schedule.
            return []
        handler = self.handlers.get(event.event_type)
        additional_records: tuple[EventLogRecord, ...] = ()
        if handler is None:
            status = EventStatus.FAILED
            detail = f"no handler registered for event type {event.event_type!r}"
        else:
            try:
                effect_event_guard_or_suppress(world, event)
                additional_records = handler.apply(world, event) or ()
            except EventSuppressed as exc:
                status = EventStatus.SUPPRESSED
                detail = str(exc)
            except EventApplicationError as exc:
                status = EventStatus.FAILED
                detail = str(exc)
            else:
                status = EventStatus.APPLIED
                detail = None
                if event.event_type in {
                    "information_contact_update",
                    "social_interaction",
                    "information_correction",
                    "proximity_contact_update",
                }:
                    from sheltersim.domain.body_state import record_social_event
                    from sheltersim.domain.social_network import (
                        SOCIAL_NETWORK_STATE_KEY,
                        observe_event,
                    )

                    record_social_event(world, event, minute)
                    if SOCIAL_NETWORK_STATE_KEY in world.model_state:
                        observe_event(world, event, minute)

        record = self.settle(world, event, minute, status, detail)
        event_outcomes = [record]
        for additional in additional_records:
            existing = world.event_log.find(additional.event.event_id)
            if existing is not None:
                if (
                    existing.event.to_dict() != additional.event.to_dict()
                    or existing.status is not additional.status
                ):
                    raise ValueError(
                        "transactional follow-up record disagrees with dispatch result"
                    )
                event_outcomes.append(existing)
            else:
                world.event_log.append(additional)
                event_outcomes.append(additional)
        if after_event is not None:
            after_event(tuple(event_outcomes))
        return event_outcomes

    def settle(
        self,
        world: WorldState,
        event: ScheduledEvent,
        minute: int,
        status: EventStatus,
        detail: str | None,
    ) -> EventLogRecord:
        """Take a handled event off the queue and record its outcome, unless its handler did."""
        if world.event_queue.find(event.event_id) is not None:
            world.event_queue.remove(event.event_id)
        record = world.event_log.find(event.event_id)
        if record is not None:
            if (
                record.event is not event and record.event.to_dict() != event.to_dict()
            ) or record.status is not status:
                raise ValueError("transactional event record disagrees with its dispatch result")
            return record
        record = EventLogRecord(
            event=event,
            realized_minute=minute,
            status=status,
            detail=detail,
        )
        world.event_log.append(record)
        return record


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
        unknown Agent. The belief strength is 0.5, or a CounterRNG draw
        ("rumor-belief-acceptance-v1") under evidence-aware-rumor-v1.
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
        CounterRNG draw ("correction-acceptance-v1"); an accepted correction relieves query pressure
        and is counted.
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
        correction under evidence-aware-rumor-v1). Draws: "contact-correction-acceptance-v1" for a
        correction and "rumor-belief-acceptance-v1" for a heard claim under evidence-aware-rumor-v1.
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

    @kernel_first("social_interaction", "apply")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        interaction_type = event.payload.get("interaction_type")
        if interaction_type is not None:
            self._apply_typed_interaction(world, event, interaction_type)
            return
        agent_id = event.payload.get("agent_id")
        request_id = event.payload.get("request_id")
        action = event.payload.get("action")
        if not all(isinstance(item, str) and item for item in (agent_id, request_id, action)):
            raise EventApplicationError(
                "social interaction requires Agent, request, and action IDs"
            )
        request = world.support_requests.get(request_id)
        agent = world.agents.get(agent_id)
        if request is None or request.agent_id != agent_id or agent is None:
            raise EventApplicationError("social interaction references an unknown request or Agent")
        if action == "offer_reassurance":
            agent.stress = max(0.0, agent.stress - 0.03)
            agent.trust = min(1.0, agent.trust + 0.01)
        elif action == "share_practical_information":
            agent.trust = min(1.0, agent.trust + 0.005)
        else:
            raise EventApplicationError("social interaction action is not allowed")

    def _apply_typed_interaction(
        self, world: WorldState, event: ScheduledEvent, interaction_type: Any
    ) -> None:
        if (
            event.kind is not EventKind.ENDOGENOUS
            or event.due_minute != world.minute_index
            or event.provenance.get("source_id") != "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1"
            or event.provenance.get("source_type") != "assumption"
            or event.source_id != event.provenance.get("policy_version")
        ):
            raise EventApplicationError("typed social interaction provenance or time is invalid")
        if event.payload.get("reason_short") is None:
            raise EventApplicationError("typed social interaction requires a reason code")
        agent_id = event.payload.get("agent_id")
        agent = world.agents.get(agent_id) if isinstance(agent_id, str) else None
        if agent is None:
            raise EventApplicationError("typed social interaction references an unknown Agent")
        if "conversation_state_context" in event.payload:
            participant_id = event.payload.get("participant_agent_id")
            participant_ids = [agent_id]
            if isinstance(participant_id, str):
                participant_ids.append(participant_id)
            try:
                validate_social_state_context(
                    event.payload["conversation_state_context"],
                    participant_ids,
                    event.due_minute,
                )
            except (TypeError, ValueError) as exc:
                raise EventApplicationError("social dialogue state context is invalid") from exc
        action = event.payload.get("action")

        if interaction_type in {"reassurance_exchange", "family_care", "voluntary_support"}:
            expected = {
                "interaction_type",
                "agent_id",
                "participant_agent_id",
                "zone_id",
                "topic_id",
                "action",
                "reason_short",
                "interaction_duration_minutes",
            }
            if _social_payload_fields(event.payload) != expected:
                raise EventApplicationError("autonomous social action payload fields do not match")
            participant_id = event.payload.get("participant_agent_id")
            participant = (
                world.agents.get(participant_id) if isinstance(participant_id, str) else None
            )
            if (
                participant is None
                or participant.id == agent.id
                or event.payload.get("zone_id") != agent.zone_id
                or participant.zone_id != agent.zone_id
            ):
                raise EventSuppressed("social action participants are no longer co-located")
            action_by_type = {
                "reassurance_exchange": ("offer_reassurance", "emotional_check_in"),
                "family_care": ("provide_family_support", "family_care"),
                "voluntary_support": ("offer_practical_help", "mutual_aid"),
            }
            expected_action, expected_topic = action_by_type[interaction_type]
            if action != expected_action or event.payload.get("topic_id") != expected_topic:
                raise EventApplicationError("social action does not match its typed topic")
            if event.payload.get("reason_short") != "autonomous_need_choice":
                raise EventApplicationError("social action reason code is not allowed")
            duration = event.payload.get("interaction_duration_minutes")
            if type(duration) is not int or not 1 <= duration <= 1440:
                raise EventApplicationError("social action duration must be within 1..1440 minutes")
            if interaction_type == "family_care" and (
                _current_household_id(world, agent.id) is None
                or _current_household_id(world, agent.id)
                != _current_household_id(world, participant.id)
            ):
                raise EventSuppressed("family support target is no longer in the same household")
            if interaction_type == "voluntary_support":
                actor_need = max(
                    agent.stress,
                    agent.needs.toilet_urgency,
                    agent.needs.hunger,
                    agent.needs.fatigue,
                    agent.needs.hydration_need,
                )
                target_need = max(
                    participant.stress,
                    participant.needs.toilet_urgency,
                    participant.needs.hunger,
                    participant.needs.fatigue,
                    participant.needs.hydration_need,
                )
                if target_need <= actor_need + 0.15:
                    raise EventSuppressed(
                        "the selected person no longer has a higher immediate need"
                    )
            body = world.model_state.get("body_state_v1", {}).get("agents", {})
            actor_body = body.get(agent.id, {})
            positive_signal = actor_body.get("positive_affect", 0.5) - actor_body.get(
                "negative_affect", 0.0
            )
            signal = min(1.0, max(-1.0, positive_signal))
            state = world.model_state.get("body_state_v1", {})
            config = state.get("config", {}) if isinstance(state, dict) else {}
            transfer_rate = config.get("social_action_affect_transfer", 0.02)
            relief = transfer_rate * signal
            participant.stress = min(1.0, max(0.0, participant.stress - relief))
            participant.trust = min(1.0, max(0.0, participant.trust + relief * 0.5))
            if interaction_type != "reassurance_exchange":
                agent.needs.fatigue = min(1.0, agent.needs.fatigue + 0.002)
                agent.trust = min(1.0, agent.trust + max(0.0, relief * 0.25))
            return

        if interaction_type == "complaint_response":
            expected = {
                "interaction_type",
                "agent_id",
                "request_id",
                "zone_id",
                "stress_threshold",
                "complaint_wait_minutes",
                "action",
                "reason_short",
            }
            if _social_payload_fields(event.payload) != expected:
                raise EventApplicationError("complaint response payload fields do not match")
            request_id = event.payload.get("request_id")
            request = world.support_requests.get(request_id)
            stress_threshold = event.payload.get("stress_threshold")
            complaint_wait = event.payload.get("complaint_wait_minutes")
            if (
                not isinstance(stress_threshold, (int, float))
                or isinstance(stress_threshold, bool)
                or not 0.0 <= stress_threshold <= 1.0
                or not isinstance(complaint_wait, int)
                or isinstance(complaint_wait, bool)
                or complaint_wait <= 0
            ):
                raise EventApplicationError("complaint response thresholds are invalid")
            if (
                request is None
                or request.agent_id != agent_id
                or request.status is not SupportRequestStatus.PENDING
                or event.payload.get("zone_id") != agent.zone_id
                or event.causation_id != request_id
                or event.due_minute - request.requested_minute < complaint_wait
            ):
                raise EventSuppressed("complaint response context is no longer current")
            if agent.stress < stress_threshold:
                raise EventSuppressed("complaint response stress candidate is no longer current")
            if event.payload.get("reason_short") not in {
                "long_wait",
                "high_stress",
                "request_still_pending",
                "rule_fallback",
            }:
                raise EventApplicationError("complaint response reason code is not allowed")
            if action == "acknowledge_complaint":
                agent.stress = max(0.0, agent.stress - 0.02)
                agent.trust = min(1.0, agent.trust + 0.01)
            elif action == "explain_staff_process":
                agent.stress = max(0.0, agent.stress - 0.01)
                agent.trust = min(1.0, agent.trust + 0.01)
            elif action == "share_practical_information":
                agent.trust = min(1.0, agent.trust + 0.005)
            else:
                raise EventApplicationError("complaint response action is not allowed")
            return

        if interaction_type == "cooperation":
            expected = {
                "interaction_type",
                "agent_id",
                "participant_agent_id",
                "zone_id",
                "stress_threshold",
                "relationship",
                "trigger_event_id",
                "action",
                "reason_short",
            }
            if _social_payload_fields(event.payload) != expected:
                raise EventApplicationError("cooperation payload fields do not match")
            participant_id = event.payload.get("participant_agent_id")
            participant = (
                world.agents.get(participant_id) if isinstance(participant_id, str) else None
            )
            trigger_event_id = event.payload.get("trigger_event_id")
            stress_threshold = event.payload.get("stress_threshold")
            if (
                not isinstance(stress_threshold, (int, float))
                or isinstance(stress_threshold, bool)
                or not 0.0 <= stress_threshold <= 1.0
            ):
                raise EventApplicationError("cooperation stress threshold is invalid")
            if participant is None or participant.id == agent.id:
                raise EventApplicationError("cooperation requires two different known Agents")
            same_group = _current_household_id(
                world, agent.id
            ) is not None and _current_household_id(world, agent.id) == _current_household_id(
                world, participant.id
            )
            if (
                event.payload.get("zone_id") != agent.zone_id
                or agent.zone_id != participant.zone_id
                or not same_group
                or event.payload.get("relationship") != "same_group"
                or event.causation_id != trigger_event_id
            ):
                raise EventSuppressed("cooperation participants or relationship changed")
            contact = (
                world.event_log.find(trigger_event_id)
                if isinstance(trigger_event_id, str)
                else None
            )
            if (
                contact is None
                or contact.status is not EventStatus.APPLIED
                or contact.event.event_type != "information_contact_update"
                or contact.realized_minute != event.due_minute - 1
                or contact.event.payload.get("sender_agent_id") != agent_id
                or contact.event.payload.get("recipient_agent_id") != participant.id
            ):
                raise EventApplicationError("cooperation does not match an applied local contact")
            if max(agent.stress, participant.stress) < stress_threshold:
                raise EventSuppressed("cooperation stress candidate is no longer current")
            if event.payload.get("reason_short") not in {
                "shared_group_contact",
                "elevated_stress",
                "rule_fallback",
            }:
                raise EventApplicationError("cooperation reason code is not allowed")
            if action != "offer_mutual_support":
                raise EventApplicationError("cooperation action is not allowed")
            agent.stress = max(0.0, agent.stress - 0.02)
            participant.stress = max(0.0, participant.stress - 0.02)
            agent.trust = min(1.0, agent.trust + 0.01)
            participant.trust = min(1.0, participant.trust + 0.01)
            return

        if interaction_type == "information_reaction":
            expected = {
                "interaction_type",
                "agent_id",
                "participant_agent_id",
                "zone_id",
                "message_id",
                "trigger_event_id",
                "action",
                "reason_short",
            }
            if _social_payload_fields(event.payload) != expected:
                raise EventApplicationError("information reaction payload fields do not match")
            participant_id = event.payload.get("participant_agent_id")
            participant = (
                world.agents.get(participant_id) if isinstance(participant_id, str) else None
            )
            message_id = event.payload.get("message_id")
            trigger_event_id = event.payload.get("trigger_event_id")
            if participant is None or participant.id == agent.id:
                raise EventApplicationError("information reaction requires two known Agents")
            if (
                event.payload.get("zone_id") != agent.zone_id
                or agent.zone_id != participant.zone_id
                or event.causation_id != trigger_event_id
            ):
                raise EventSuppressed("information reaction contact is no longer local")
            contact = (
                world.event_log.find(trigger_event_id)
                if isinstance(trigger_event_id, str)
                else None
            )
            if (
                contact is None
                or contact.status is not EventStatus.APPLIED
                or contact.event.event_type != "information_contact_update"
                or contact.realized_minute != event.due_minute - 1
                or contact.event.payload.get("recipient_agent_id") != agent_id
                or contact.event.payload.get("sender_agent_id") != participant_id
            ):
                raise EventApplicationError(
                    "information reaction does not match an applied contact"
                )
            state = world.model_state.get(INFORMATION_SPREAD_STATE_KEY)
            if not isinstance(state, dict):
                raise EventSuppressed("information state is no longer enabled")
            raw_message = state["messages"].get(message_id)
            raw_belief = state["beliefs"].get(agent_id, {}).get(message_id)
            if raw_message is None or raw_belief is None:
                raise EventSuppressed("information claim is no longer current")
            message = TypedMessage.from_dict(raw_message)
            belief = AgentInformationState.from_dict(raw_belief)
            if (
                message.truth_status is not InformationTruthStatus.FALSE
                or belief.belief is not InformationBeliefStatus.HEARD
            ):
                raise EventSuppressed("information claim has already been verified or rejected")
            if action != "seek_claim_verification":
                raise EventApplicationError("information reaction action is not allowed")
            if event.payload.get("reason_short") not in {
                "heard_through_contact",
                "request_verification",
                "rule_fallback",
            }:
                raise EventApplicationError("information reaction reason code is not allowed")
            config = InformationSpreadConfig.from_mapping(world.model_configs["information_spread"])
            state["query_pressure"][agent_id] = min(
                1.0,
                state["query_pressure"][agent_id] + config.query_pressure_per_claim,
            )
            return

        raise EventApplicationError("typed social interaction type is not allowed")


@dataclass(frozen=True)
class EffectProgramPopulationHandler:
    """Apply typed population primitives through the approved EffectProgram record."""

    @kernel_first("handlers", "population")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        from sheltersim.core.world import WorldState as MutableWorldState
        from sheltersim.domain.population import (
            apply_arrival_approach_completion,
            queue_arrivals,
            queue_departures,
        )
        from sheltersim.domain.population_lifecycle import (
            apply_population_presence_command,
        )
        from sheltersim.domain.simulator_state import validate_simulator_extensions

        if event.event_type == "population_approach_complete":
            apply_arrival_approach_completion(world, event)
            return
        op = event.payload.get("op")
        expected_base = {"program_id", "effect_id", "op", "zone_id", "count"}
        if op == "add_evacuees":
            expected = expected_base | {
                "household_size",
                "mobility",
                "language",
                "admission_zone_ids",
                "approach_route_id",
                "person_context_overrides",
            }
            previous_expected = expected - {"person_context_overrides"}
            prior_arrival_expected = previous_expected - {"approach_route_id"}
            legacy_expected = expected_base | {"household_size", "mobility", "language"}
        elif op == "depart_evacuees":
            expected = expected_base | {"include_initial"}
            legacy_expected = expected_base
        elif op == "population_presence":
            expected = {"program_id", "effect_id", "op", "command"}
        else:
            raise EventApplicationError("population primitive operation is invalid")
        if set(event.payload) != expected and not (
            op == "add_evacuees"
            and frozenset(event.payload)
            in {
                frozenset(previous_expected),
                frozenset(prior_arrival_expected),
                frozenset(legacy_expected),
            }
            or op == "depart_evacuees"
            and set(event.payload) == legacy_expected
        ):
            raise EventApplicationError("population primitive payload fields do not match")
        state = world.model_state.get("effect_program_v1")
        program = (
            state.get("programs", {}).get(event.payload.get("program_id"))
            if isinstance(state, dict)
            else None
        )
        effect_id = event.payload.get("effect_id")
        if (
            not isinstance(program, dict)
            or effect_id not in program.get("effect_ids", [])
            or program.get("operators", {}).get(effect_id) != op
            or event.event_id not in program.get("scheduled_event_ids", [])
            or (op != "population_presence" and event.payload.get("zone_id") not in world.zones)
        ):
            raise EventApplicationError("population primitive is not in the approved program")
        if op == "population_presence":
            apply_population_presence_command(
                world,
                event,
                event.payload["command"],
            )
            return
        candidate = MutableWorldState.from_dict(world.to_dict())
        try:
            if op == "add_evacuees":
                queue_arrivals(
                    candidate,
                    event=event,
                    zone_id=event.payload["zone_id"],
                    count=event.payload["count"],
                    household_size=event.payload["household_size"],
                    mobility=event.payload["mobility"],
                    language=event.payload["language"],
                    admission_zone_ids=tuple(event.payload.get("admission_zone_ids", ())),
                    approach_route_id=event.payload.get("approach_route_id"),
                    person_context_overrides=tuple(
                        event.payload.get("person_context_overrides", ())
                    ),
                )
            else:
                queue_departures(
                    candidate,
                    event=event,
                    zone_id=event.payload["zone_id"],
                    count=event.payload["count"],
                    include_initial=event.payload.get("include_initial", False),
                )
                from sheltersim.domain.population_lifecycle import admit_pending_returns

                admit_pending_returns(candidate, event.due_minute)
        except EventSuppressed:
            candidate.validate()
            validate_simulator_extensions(candidate)
            world.__dict__.clear()
            world.__dict__.update(candidate.__dict__)
            raise
        except EventApplicationError:
            raise
        except (TypeError, ValueError) as exc:
            raise EventApplicationError(str(exc)) from exc
        candidate.validate()
        validate_simulator_extensions(candidate)
        world.__dict__.clear()
        world.__dict__.update(candidate.__dict__)


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

    @kernel_first("travel_completion", "apply")
    def apply(self, world: WorldState, event: ScheduledEvent) -> None:
        agent_id = event.payload.get("agent_id")
        task_id = event.payload.get("task_id")
        destination_zone_id = event.payload.get("destination_zone_id")
        purpose = event.payload.get("purpose")
        facility_id = event.payload.get("facility_id")
        origin_zone_id = event.payload.get("origin_zone_id")
        if not isinstance(origin_zone_id, str):
            route_zones = event.payload.get("route_zone_ids", ())
            origin_zone_id = (
                route_zones[0] if isinstance(route_zones, (list, tuple)) and route_zones else None
            )
        if not all(
            isinstance(item, str) and item
            for item in (agent_id, task_id, destination_zone_id, purpose)
        ):
            raise EventApplicationError(
                "travel completion requires Agent, task, destination, and purpose"
            )
        task = world.active_tasks.get(task_id)
        agent = world.agents.get(agent_id)
        queue_purpose = purpose in {
            "toilet_queue_position",
            "resource_queue_position",
            "toilet_queue_service",
            "resource_queue_service",
        }
        if task is None or task.kind not in (
            "travel_to_toilet",
            "travel_to_resource",
            "travel_return",
            "staff_support_travel",
            "toilet_queue",
            "resource_service_queue",
        ):
            raise EventApplicationError("travel completion does not match an active task")
        if agent is None or task.agent_id != agent_id:
            raise EventApplicationError("travel completion Agent does not match its active task")
        if queue_purpose:
            resource = purpose.startswith("resource_")
            expected_kind = "resource_service_queue" if resource else "toilet_queue"
            facility = world.facilities.get(facility_id) if isinstance(facility_id, str) else None
            queue = (
                facility.resource_queue_agent_ids
                if resource and facility is not None
                else facility.queue_agent_ids
                if facility is not None
                else ()
            )
            movement = world.model_state.get("movement_v1", {})
            journey = (
                movement.get("journeys", {}).get(agent_id, {}) if isinstance(movement, dict) else {}
            )
            from sheltersim.domain.facility_queues import (
                FACILITY_QUEUES_STATE_KEY,
                facility_has_independent_queues,
            )

            independent = isinstance(facility_id, str) and facility_has_independent_queues(
                world, facility_id
            )
            request_id = event.payload.get("request_id")
            queue_id = event.payload.get("queue_id")
            if independent:
                queue_state = world.model_state[FACILITY_QUEUES_STATE_KEY]
                request = (
                    queue_state["requests"].get(request_id) if isinstance(request_id, str) else None
                )
                queue_row = (
                    queue_state["queues"].get(queue_id) if isinstance(queue_id, str) else None
                )
                queue_contains_request = (
                    isinstance(request, dict)
                    and isinstance(queue_row, dict)
                    and request_id in queue_row["queued_request_ids"]
                    and request["agent_id"] == agent_id
                    and request["facility_id"] == facility_id
                    and request["queue_id"] == queue_id
                )
                service_point_id = (
                    request.get("service_point_id") if isinstance(request, dict) else None
                )
                at_reserved_destination = (
                    agent_is_at_queue_slot(world, agent_id, facility_id, queue_id)
                    or agent_is_at_queue_trip_destination(world, agent_id, journey)
                    if purpose.endswith("_position") and isinstance(facility_id, str)
                    else agent_is_at_facility_service_point(
                        world, agent_id, facility_id, service_point_id
                    )
                    if isinstance(facility_id, str) and isinstance(service_point_id, str)
                    else False
                )
            else:
                queue_contains_request = agent_id in queue
                at_reserved_destination = (
                    agent_is_at_queue_slot(world, agent_id, facility_id)
                    or agent_is_at_queue_trip_destination(world, agent_id, journey)
                    if purpose.endswith("_position") and isinstance(facility_id, str)
                    else agent_is_at_facility_service_point(world, agent_id, facility_id)
                    if isinstance(facility_id, str)
                    else False
                )
            if (
                task.kind != expected_kind
                or task.due_minute != task.start_minute
                or agent.activity is not Activity.QUEUED
                or agent.target_id != facility_id
                or not queue_contains_request
                or facility is None
                or facility.zone_id != destination_zone_id
                or agent.zone_id != destination_zone_id
                or not at_reserved_destination
                or (
                    journey
                    and (
                        journey.get("event_id") != event.event_id
                        or journey.get("task_id") != task_id
                        or journey.get("purpose") != purpose
                        or journey.get("due_minute") != event.due_minute
                        or (independent and journey.get("queue_id") != queue_id)
                    )
                )
                or (not journey and event.due_minute != world.minute_index)
            ):
                raise EventApplicationError(
                    "physical queue travel does not match its FIFO reservation"
                )
            return
        if task.due_minute != event.due_minute:
            raise EventApplicationError("travel completion does not match an active due task")
        if agent.activity is not Activity.MOVING:
            raise EventApplicationError("travel completion Agent does not match its active task")
        if destination_zone_id not in world.zones:
            raise EventApplicationError(f"destination zone {destination_zone_id!r} does not exist")
        if purpose == "toilet_request":
            if not isinstance(facility_id, str) or not facility_id:
                raise EventApplicationError("toilet travel completion requires a facility_id")
            facility = world.facilities.get(facility_id)
            if facility is None or facility.zone_id != destination_zone_id:
                raise EventApplicationError("toilet travel destination does not match its facility")
            if task.kind != "travel_to_toilet" or agent.target_id != facility_id:
                raise EventApplicationError("toilet travel completion does not match its target")
        elif purpose == "resource_request":
            if self.resource_service is None:
                raise EventApplicationError("resource travel is not enabled for this scenario")
            if not isinstance(facility_id, str) or not facility_id:
                raise EventApplicationError("resource travel completion requires a facility_id")
            route_zone_ids = event.payload.get("route_zone_ids")
            facility = world.facilities.get(facility_id)
            if (
                facility is None
                or facility.zone_id != destination_zone_id
                or not isinstance(origin_zone_id, str)
                or origin_zone_id not in world.zones
                or not isinstance(route_zone_ids, (list, tuple))
                or not route_zone_ids
                or route_zone_ids[0] != origin_zone_id
                or route_zone_ids[-1] != destination_zone_id
                or ResourceServiceManager.resource_for_facility(facility.kind) is None
                or task.kind != "travel_to_resource"
                or agent.target_id != facility_id
            ):
                raise EventApplicationError(
                    "resource travel completion does not match its target facility"
                )
        elif purpose in {"return", "queue_abandon"}:
            if task.kind != "travel_return" or agent.target_id != destination_zone_id:
                raise EventApplicationError("return travel completion does not match its target")
        elif purpose == "staff_support_travel":
            request_id = event.payload.get("request_id")
            staff_id = event.payload.get("staff_id")
            request = (
                world.support_requests.get(request_id) if isinstance(request_id, str) else None
            )
            staff_member = world.staff.get(staff_id) if isinstance(staff_id, str) else None
            if (
                task.kind != "staff_support_travel"
                or request is None
                or request.agent_id != agent_id
                or request.status is not SupportRequestStatus.ASSIGNED
                or request.assigned_staff_id != staff_id
                or request.task_id is None
                or agent.support_request_id != request.id
                or staff_member is None
                or staff_member.zone_id != destination_zone_id
                or agent.target_id != destination_zone_id
                or task.due_minute != event.due_minute
            ):
                raise EventApplicationError(
                    "staff support travel does not match its assigned request and staff member"
                )
        elif purpose == "sleep_bed":
            overlay = world.model_state.get("geometry_overlay_v1", {})
            reservations = overlay.get("bed_reservations", {}) if isinstance(overlay, dict) else {}
            reservation = reservations.get(agent_id) if isinstance(reservations, dict) else None
            rooms = overlay.get("temporary_rooms", {}) if isinstance(overlay, dict) else {}
            room = rooms.get(reservation.get("room_id")) if isinstance(reservation, dict) else None
            movement = world.model_state.get(MOVEMENT_STATE_KEY, {})
            journey = (
                movement.get("journeys", {}).get(agent_id) if isinstance(movement, dict) else None
            )
            bed_index = reservation.get("bed_index") if isinstance(reservation, dict) else None
            bed = (
                room.get("bed_points_m", [])[bed_index]
                if isinstance(room, dict)
                and type(bed_index) is int
                and 0 <= bed_index < len(room.get("bed_points_m", []))
                else None
            )
            route_points = journey.get("route_points", []) if isinstance(journey, dict) else []
            if (
                task.kind != "travel_return"
                or agent.target_id != destination_zone_id
                or not isinstance(reservation, dict)
                or not isinstance(room, dict)
                or room.get("zone_id") != destination_zone_id
                or not isinstance(journey, dict)
                or journey.get("event_id") != event.event_id
                or journey.get("task_id") != task_id
                or journey.get("purpose") != "sleep_bed"
                or not route_points
                or route_points[-1] != bed
            ):
                raise EventApplicationError("sleep-bed travel does not match its live reservation")
        elif purpose == "presence_relocation":
            from sheltersim.domain.population_lifecycle import POPULATION_LIFECYCLE_STATE_KEY

            lifecycle = world.model_state.get(POPULATION_LIFECYCLE_STATE_KEY, {})
            relocation = lifecycle.get("pending_relocations", {}).get(agent_id)
            if (
                task.kind != "travel_return"
                or agent.target_id != destination_zone_id
                or not isinstance(relocation, dict)
                or relocation.get("task_id") != task_id
                or relocation.get("event_id") != event.event_id
                or relocation.get("from_zone_id") != origin_zone_id
                or relocation.get("to_zone_id") != destination_zone_id
            ):
                raise EventApplicationError(
                    "presence relocation completion does not match its route"
                )
        else:
            raise EventApplicationError(f"unsupported travel purpose {purpose!r}")

        if purpose == "sleep_bed":
            movement["positions"][agent_id].update(
                {
                    "zone_id": destination_zone_id,
                    "point": bed,
                    "edge_id": None,
                    "progress": 1.0,
                    "position_kind": "physical",
                }
            )
        del world.active_tasks[task_id]
        agent.zone_id = destination_zone_id
        # A person who walked to a temporary room's bed chose to sleep, so they
        # sleep on arrival instead of idling until their next decision (#84).
        agent.activity = (
            Activity.SEEKING_HELP
            if purpose == "staff_support_travel"
            else Activity.SLEEPING
            if purpose == "sleep_bed"
            else Activity.IDLE
        )
        agent.target_id = None
        if purpose == "presence_relocation":
            from sheltersim.domain.population_lifecycle import complete_presence_relocation

            complete_presence_relocation(world, event)
        self._after_arrival(
            world, event, agent_id, purpose, facility_id, destination_zone_id, origin_zone_id
        )

    @kernel_first("travel_completion", "after_arrival")
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
        """What a walk to a service leads to. rules/travel_completion.rs has the kernel."""
        if (
            purpose in {"toilet_request", "resource_request"}
            and "facility_geometry" in world.model_configs
            and isinstance(facility_id, str)
        ):
            from sheltersim.domain.facility_queues import facility_has_independent_queues

            if not facility_has_independent_queues(world, facility_id):
                approach = facility_approach_route(world, agent_id, facility_id)
                if approach is None:
                    self._reject_room_approach(world, event, agent_id, facility_id, origin_zone_id)
                    return
                local_route, route_points = approach
                if local_route.travel_minutes > 0:
                    if self._schedule_room_approach(
                        world,
                        agent_id=agent_id,
                        facility_id=facility_id,
                        purpose=purpose,
                        route=local_route,
                        route_points=route_points,
                        minute=world.minute_index,
                        origin_zone_id=destination_zone_id,
                    ):
                        return
                    self._reject_room_approach(world, event, agent_id, facility_id, origin_zone_id)
                    return
        if purpose == "toilet_request":
            try:
                self.toilet_service.request(
                    agent_id,
                    facility_id,
                    minute=world.minute_index,
                    validate_state=self.validate_request_state,
                )
            except ValueError as exc:
                raise EventApplicationError(str(exc)) from exc
        elif purpose == "resource_request":
            try:
                self.resource_service.request(
                    agent_id,
                    facility_id,
                    minute=world.minute_index,
                    validate_state=False,
                )
            except ValueError as exc:
                # Arrival has completed; failed admission leaves the Agent idle at the facility.
                facility = world.facilities[facility_id]
                resource_id = ResourceServiceManager.resource_for_facility(facility.kind)
                action = "fetch_water" if resource_id == "water_ml" else "collect_meal"
                self.resource_service.record_request_outcome(
                    agent_id=agent_id,
                    action=action,
                    outcome="rejected",
                    reason="request_rejected",
                    facility_id=facility_id,
                    origin_zone_id=origin_zone_id,
                    detail=str(exc),
                )
                world.metric_state["resource_service_request_rejected_count"] = (
                    world.metric_state.get("resource_service_request_rejected_count", 0.0) + 1.0
                )

    def _schedule_room_approach(
        self,
        world: WorldState,
        *,
        agent_id: str,
        facility_id: str,
        purpose: str,
        route: Any,
        route_points: tuple[Any, ...],
        minute: int,
        origin_zone_id: str,
    ) -> bool:
        due_minute = minute + route.travel_minutes
        if due_minute >= world.duration_minutes:
            return False
        sequence = world.allocate_event_sequence()
        task_id = f"agent-facility-approach-{sequence}"
        event_id = f"agent-facility-approach-complete-{sequence}"
        task = ActiveTask(
            id=task_id,
            kind="travel_to_toilet" if purpose == "toilet_request" else "travel_to_resource",
            agent_id=agent_id,
            resource_id=None,
            start_minute=minute,
            due_minute=due_minute,
        )
        event = ScheduledEvent(
            event_id=event_id,
            kind=EventKind.COMPLETION,
            due_minute=due_minute,
            priority=0,
            sequence=sequence,
            event_type="agent_travel_complete",
            payload={
                "agent_id": agent_id,
                "task_id": task_id,
                "purpose": purpose,
                "destination_zone_id": route.destination_zone_id,
                "facility_id": facility_id,
                "origin_zone_id": origin_zone_id,
                "route_zone_ids": list(route.zone_ids),
                "route_edge_ids": list(route.edge_ids),
                "travel_minutes": route.travel_minutes,
            },
            source_id="agent-facility-approach-v2",
            causation_id=None,
            provenance={
                "source_type": "assumption",
                "source_id": "ASM-SPATIAL-MOVEMENT-V2",
                "policy_version": "spatial-route-and-room-waypoint-v2",
            },
        )
        world.active_tasks[task_id] = task
        agent = world.agents[agent_id]
        agent.activity = Activity.MOVING
        agent.target_id = facility_id
        world.schedule_event(event)
        if MOVEMENT_STATE_KEY in world.model_state:
            start_movement(
                world,
                agent_id=agent_id,
                route=route,
                purpose=purpose,
                facility_id=facility_id,
                started_minute=minute,
                due_minute=due_minute,
                event_id=event_id,
                task_id=task_id,
                route_points=route_points,
            )
        return True

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

    @kernel_first("support", "complete_support")
    def apply(self, world: WorldState, event: ScheduledEvent) -> tuple[EventLogRecord, ...] | None:
        values = tuple(
            event.payload.get(key) for key in ("request_id", "agent_id", "staff_id", "task_id")
        )
        if not all(isinstance(value, str) and value for value in values):
            raise EventApplicationError(
                "staff completion requires request, Agent, staff, and task IDs"
            )
        request_id, agent_id, staff_id, task_id = values
        task = world.active_tasks.get(task_id)
        operational = world.model_state.get(OPERATIONAL_EVENTS_KEY)
        if (
            task is None
            and operational is not None
            and task_id in operational["cancelled_staff_task_ids"]
        ):
            raise EventSuppressed(f"staff support task {task_id!r} was cancelled")
        if task is None or task.due_minute != event.due_minute:
            raise EventApplicationError("staff completion does not match an active due task")
        care_request = care_receipt_for_request(world, request_id)
        before: list[tuple[str, str, str, float, str]] = []
        agent = world.agents.get(agent_id)
        from sheltersim.policies.registry import create_care_outcome_formula

        care_formula = create_care_outcome_formula(world.model_configs)
        typed_care_deltas = (
            care_formula.support_state_deltas(
                care_request.get("support_kind", ""),
                agent_id=agent.id if agent is not None else None,
            )
            if care_formula is not None and care_request is not None
            else {}
        )
        relational_care_deltas: dict[str, dict[str, float]] = {}
        relational_context_source_ids: set[str] = set()
        if (
            care_request is not None
            and care_request.get("support_kind") == "caregiver_assistance"
            and care_formula is not None
        ):
            from sheltersim.domain.care_receipts import explicit_care_dependents

            relational_care_deltas = {
                dependent_id: care_formula.support_state_deltas(
                    "caregiver_assistance", agent_id=dependent_id
                )
                for dependent_id in explicit_care_dependents(
                    world,
                    agent_id,
                    household_id=care_request.get("household_id"),
                )
            }
            relational_care_deltas = {
                identity: deltas for identity, deltas in relational_care_deltas.items() if deltas
            }
            from sheltersim.contracts.population import population_context_people

            people_context = population_context_people(world)
            for identity in (agent_id, *relational_care_deltas):
                context = people_context.get(identity, {})
                sources = context.get("source_ids", ()) if isinstance(context, dict) else ()
                relational_context_source_ids.update(
                    source for source in sources if isinstance(source, str)
                )
        if care_request is not None and agent is not None:
            body_state = world.model_state.get("body_state_v1")
            body = (
                body_state.get("agents", {}).get(agent.id) if isinstance(body_state, dict) else None
            )
            if care_request.get("support_kind") == "emotional_support":
                before.extend(
                    (
                        ("agent", agent.id, "stress", agent.stress, "normalized_0_1"),
                        ("agent", agent.id, "trust", agent.trust, "normalized_0_1"),
                    )
                )
            if (
                isinstance(body_state, dict)
                and isinstance(body, dict)
                and (
                    care_request.get("support_kind") == "emotional_support"
                    or care_request.get("support_kind") == "hygiene_support"
                    or typed_care_deltas
                )
            ):
                for field in ("reassurance", "positive_affect", "negative_affect"):
                    if care_request.get("support_kind") == "emotional_support":
                        before.append(
                            (
                                "model_state",
                                agent.id,
                                f"body_state_v1.agents.{field}",
                                float(body[field]),
                                "normalized_0_1",
                            )
                        )
                if care_request.get("support_kind") == "hygiene_support":
                    before.append(
                        (
                            "model_state",
                            agent.id,
                            "body_state_v1.agents.hygiene_need",
                            float(body["hygiene_need"]),
                            "normalized_0_1",
                        )
                    )
                for path in typed_care_deltas:
                    if path == "agent.stress":
                        before.append(
                            (
                                "agent",
                                agent.id,
                                "stress",
                                float(agent.stress),
                                "normalized_0_1",
                            )
                        )
                    elif path.startswith("body_state_v1.agents."):
                        field = path.rsplit(".", 1)[-1]
                        before.append(
                            (
                                "model_state",
                                agent.id,
                                path,
                                float(body[field]),
                                "normalized_0_1",
                            )
                        )
        for dependent_id, deltas in relational_care_deltas.items():
            dependent = world.agents[dependent_id]
            dependent_body = world.model_state["body_state_v1"]["agents"][dependent_id]
            for path in deltas:
                if path == "agent.stress":
                    before.append(
                        (
                            "agent",
                            dependent_id,
                            "stress",
                            float(dependent.stress),
                            "normalized_0_1",
                        )
                    )
                elif path.startswith("body_state_v1.agents."):
                    field = path.rsplit(".", 1)[-1]
                    before.append(
                        (
                            "model_state",
                            dependent_id,
                            path,
                            float(dependent_body[field]),
                            "normalized_0_1",
                        )
                    )
        try:
            complete_support_task(
                world,
                request_id=request_id,
                agent_id=agent_id,
                staff_id=staff_id,
                task_id=task_id,
                minute=world.minute_index,
                cooldown_minutes=self.cooldown_minutes,
                require_colocated=self.require_colocated,
            )
        except ValueError as exc:
            raise EventApplicationError(str(exc)) from exc
        if care_request is None:
            return None

        observation = "not_measured"
        changes: list[NumericStateChange] = []
        care_outcome_source_id: str | None = None
        if agent is not None and (
            care_request.get("support_kind") in {"emotional_support", "hygiene_support"}
            or typed_care_deltas
        ):
            body_state = world.model_state.get("body_state_v1")
            body = (
                body_state.get("agents", {}).get(agent.id) if isinstance(body_state, dict) else None
            )
            config = body_state.get("config", {}) if isinstance(body_state, dict) else {}
            if isinstance(body, dict):
                support_kind = care_request.get("support_kind")
                if support_kind == "emotional_support":
                    relief = float(
                        config.get("complaint_reassurance_gain_by_agent", {}).get(
                            agent.id,
                            config.get("complaint_reassurance_gain", 0.0),
                        )
                    )
                    agent.stress = max(0.0, agent.stress - relief)
                    agent.trust = min(1.0, agent.trust + relief * 0.25)
                    body["reassurance"] = min(1.0, body["reassurance"] + relief)
                    body["positive_affect"] = min(1.0, body["positive_affect"] + relief * 0.5)
                    body["negative_affect"] = max(0.0, body["negative_affect"] - relief * 0.5)
                elif support_kind == "hygiene_support":
                    relief = float(
                        config.get("hygiene_support_relief_by_agent", {}).get(
                            agent.id,
                            config.get("hygiene_support_relief", 0.0),
                        )
                    )
                    from sheltersim.policies.registry import create_care_outcome_formula

                    care_formula = create_care_outcome_formula(world.model_configs)
                    if care_formula is None:
                        body["hygiene_need"] = max(0.0, body["hygiene_need"] - relief)
                    else:
                        body["hygiene_need"] = care_formula.hygiene_need_after_support(
                            float(body["hygiene_need"]), relief
                        )
                        care_outcome_source_id = care_formula.source_id
                elif typed_care_deltas and care_formula is not None:
                    for path, delta in typed_care_deltas.items():
                        if path == "agent.stress":
                            agent.stress = min(1.0, max(0.0, agent.stress + delta))
                        elif path.startswith("body_state_v1.agents."):
                            field = path.rsplit(".", 1)[-1]
                            body[field] = min(1.0, max(0.0, body[field] + delta))
                    if typed_care_deltas:
                        care_outcome_source_id = care_formula.source_id
                if care_formula is not None:
                    for dependent_id, deltas in relational_care_deltas.items():
                        dependent = world.agents[dependent_id]
                        dependent_body = body_state["agents"][dependent_id]
                        for path, delta in deltas.items():
                            if path == "agent.stress":
                                dependent.stress = min(1.0, max(0.0, dependent.stress + delta))
                            elif path.startswith("body_state_v1.agents."):
                                field = path.rsplit(".", 1)[-1]
                                dependent_body[field] = min(
                                    1.0, max(0.0, dependent_body[field] + delta)
                                )
                        if deltas:
                            care_outcome_source_id = care_formula.source_id
                after_values = {
                    (kind, identity, path): (
                        getattr(world.agents[identity], path)
                        if kind == "agent"
                        else world.model_state["body_state_v1"]["agents"][identity][
                            path.rsplit(".", 1)[-1]
                        ]
                    )
                    for kind, identity, path, _, _ in before
                }
                for kind, identity, path, previous, unit in before:
                    changes.append(
                        NumericStateChange(
                            entity_kind=kind,
                            entity_id=identity,
                            field_path=path,
                            before=previous,
                            after=after_values[(kind, identity, path)],
                            unit=unit,
                        )
                    )
                observation = (
                    "observed_change"
                    if any(change.before != change.after for change in changes)
                    else "observed_no_change"
                )

        care_source_ids = [
            "ASM-STAFF-SUPPORT-LIFECYCLE-V1",
            *sorted(relational_context_source_ids),
        ]
        if observation != "not_measured":
            care_source_ids.append("ASM-LIFE-STATE-FEEDBACK-V1")
        if care_outcome_source_id is not None:
            care_source_ids.append(care_outcome_source_id)
        care_event, _ = make_care_transition_record(
            world,
            request_id=request_id,
            agent_id=agent_id,
            household_id=care_request.get("household_id"),
            support_kind=care_request["support_kind"],
            status="completed",
            staff_id=staff_id,
            outcome_observation=observation,
            state_changes=tuple(changes),
            causation_id=event.event_id,
            source_ids=tuple(care_source_ids),
        )
        return (care_event,)


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

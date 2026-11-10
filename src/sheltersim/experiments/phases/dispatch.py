"""The dispatch phase: apply what is due, then what follows from the applied events."""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import read_only_state
from sheltersim.core.world import WorldState

# native/src/rules/dispatch.rs calls these four through this module, so a test's patch here shows.
from sheltersim.domain.action_lifecycle import (  # noqa: F401
    record_applied_action_completion,
    record_applied_population_exit,
)
from sheltersim.domain.agent_dynamics import learn_facility  # noqa: F401
from sheltersim.domain.care_receipts import ingest_applied_shared_operation_records
from sheltersim.domain.information import INFORMATION_SPREAD_STATE_KEY
from sheltersim.domain.models import (
    Activity,
    AgentInformationState,
    InformationBeliefStatus,
    InformationTruthStatus,
    TypedMessage,
)
from sheltersim.domain.movement import MOVEMENT_STATE_KEY, finish_movement
from sheltersim.domain.population import admit_pending_arrivals  # noqa: F401
from sheltersim.domain.population_lifecycle import current_household_id
from sheltersim.domain.staff_operations import interrupt_staff_trips_on_closed_routes
from sheltersim.events.models import ScheduledEvent


class DispatchPhase:
    """Steps of TickPhase.DISPATCH_DUE_EVENTS, a mixin of OfflineSimulationController."""

    def _run_dispatch_phase(self, world: WorldState, minute: int) -> None:
        """Apply what is due at `minute` and what follows: native/src/rules/dispatch.rs.

        Each event due now goes through `event_dispatcher` in queue order, the shared operation
        records of each one to `ingest_applied_shared_operation_records`; an event the table of
        dispatch.rs has no Rust function for, or that changed in the queue, through
        `_dispatch_one_event`. Then, for each APPLIED event: `record_applied_action_completion` and
        `record_applied_population_exit`; for `agent_travel_complete`, `learn_facility` of its
        person and facility when both are str, and `_record_staff_support_started` for a
        `staff_support_travel` trip; for `support_request_timeout`, `_record_request_interrupted`.
        `_refresh_space` once if a geometry or structure event applied, `_interrupt_trips_of_event`
        for each applied geometry event, `_admit_pending_returns`, `_sync_movement_after_events`,
        `admit_pending_arrivals`, and `_consider_social_contact_event` for each applied
        `information_contact_update`.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        before the first event raises KernelRefusal: a dispatcher of another class, a negative
        minute, an event queue or counters it cannot read. Past that, a refusal is a RuntimeError.
        """

    def _dispatch_one_event(
        self, world: WorldState, event: ScheduledEvent, minute: int
    ) -> list[Any]:
        """What native/src/rules/dispatch.rs calls for an event it has no Rust function for."""
        return self.event_dispatcher.dispatch_one(
            world,
            event,
            minute,
            after_event=lambda records: ingest_applied_shared_operation_records(world, records),
        )

    @kernel_only("support", "record_started")
    def _record_staff_support_started(self, world: WorldState, event: ScheduledEvent) -> None:
        """Append the "started" care transition of a staff trip's arrival at its request.

        Only for a request with a care receipt that is still ASSIGNED to the trip's staff member,
        whose person is SEEKING_HELP. The transition keeps the household the receipt was made in,
        as the completion does; a later reunion must not change it (#179). Its causation is the
        request's task, its staff the assigned one, and its sources the person's. Nothing for an
        event without a request.
        """

    @kernel_only("support", "record_interrupted")
    def _record_request_interrupted(self, world: WorldState, event: ScheduledEvent) -> None:
        """Append the "interrupted" care transition of an applied support_request_timeout.

        Only for an UNRESOLVED request with a care receipt; the receipt's household and kind, the
        timeout event as the causation, and the person's sources. Nothing for an event without a
        request.
        """

    def _interrupt_trips_of_event(self, world: WorldState, event: ScheduledEvent) -> None:
        parameters = event.payload.get("parameters", {})
        if (
            event.event_type == "effect_program_geometry"
            and event.payload.get("op") == "set_route_access"
            and isinstance(parameters, Mapping)
            and parameters.get("accessible") is False
        ):
            interrupt_staff_trips_on_closed_routes(world, event)

    def _admit_pending_returns(self, world: WorldState, minute: int) -> None:
        from sheltersim.domain.population_lifecycle import admit_pending_returns

        admit_pending_returns(world, minute)

    def _sync_movement_after_events(self) -> None:
        """Finish routes when their typed travel-completion event changed Agent state."""
        state = self.world.model_state.get(MOVEMENT_STATE_KEY)
        if state is None:
            return
        for agent_id in sorted(tuple(state["journeys"])):
            agent = self.world.agents.get(agent_id)
            journey = state["journeys"].get(agent_id, {})
            queue_trip_is_active = (
                agent is not None
                and agent.activity is Activity.QUEUED
                and journey.get("purpose")
                in {
                    "toilet_queue_position",
                    "resource_queue_position",
                    "toilet_queue_service",
                    "resource_queue_service",
                }
            )
            if agent is None or (
                agent.activity is not Activity.MOVING and not queue_trip_is_active
            ):
                self._finish_stopped_journey(agent_id)

    def _finish_stopped_journey(self, agent_id: str) -> None:
        state = self.world.model_state[MOVEMENT_STATE_KEY]
        agent = self.world.agents.get(agent_id)
        journey = state["journeys"].get(agent_id, {})
        bed_point = None
        if journey.get("purpose") == "sleep_bed":
            overlay = read_only_state(self.world, "geometry_overlay_v1", {})
            reservation = overlay.get("bed_reservations", {}).get(agent_id, {})
            room = overlay.get("temporary_rooms", {}).get(reservation.get("room_id"), {})
            bed_points = room.get("bed_points_m", [])
            index = reservation.get("bed_index")
            if type(index) is int and 0 <= index < len(bed_points):
                bed_point = bed_points[index]
        finish_movement(self.world, agent_id)
        if bed_point is not None and agent is not None:
            state["positions"][agent_id].update(
                {
                    "zone_id": agent.zone_id,
                    "point": bed_point,
                    "edge_id": None,
                    "progress": 1.0,
                    "position_kind": "physical",
                }
            )

    def _consider_social_contact_event(self, event: ScheduledEvent) -> None:
        if self.social_policy is None or event.event_type != "information_contact_update":
            return
        sender_id = event.payload.get("sender_agent_id")
        recipient_id = event.payload.get("recipient_agent_id")
        message_id = event.payload.get("message_id")
        if not all(isinstance(value, str) for value in (sender_id, recipient_id)):
            return
        sender = self.world.agents.get(sender_id)
        recipient = self.world.agents.get(recipient_id)
        if sender is None or recipient is None or sender_id == recipient_id:
            return
        if (
            current_household_id(self.world, sender.id) is not None
            and current_household_id(self.world, sender.id)
            == current_household_id(self.world, recipient.id)
            and max(sender.stress, recipient.stress) >= self.social_policy.limits.stress_threshold
        ):
            self.social_policy.consider_cooperation(
                self.world,
                initiator_agent_id=sender_id,
                participant_agent_id=recipient_id,
                trigger_event_id=event.event_id,
            )
        state = self.world.model_state.get(INFORMATION_SPREAD_STATE_KEY)
        if not isinstance(message_id, str) or not isinstance(state, dict):
            return
        raw_message = state["messages"].get(message_id)
        raw_belief = state["beliefs"].get(recipient_id, {}).get(message_id)
        if raw_message is None or raw_belief is None:
            return
        message = TypedMessage.from_dict(raw_message)
        belief = AgentInformationState.from_dict(raw_belief)
        if (
            message.truth_status is InformationTruthStatus.FALSE
            and belief.belief is InformationBeliefStatus.HEARD
        ):
            self.social_policy.consider_information_reaction(
                self.world,
                agent_id=recipient_id,
                participant_agent_id=sender_id,
                message_id=message_id,
                trigger_event_id=event.event_id,
            )

    @kernel_only("travel", "schedule_return")
    def _schedule_return_to_initial_zone(
        self, world: WorldState, service_event: ScheduledEvent
    ) -> None:
        """Walk the event's person back to their initial zone after a service.

        Nothing for an unknown person, one already there, or no route (`_route`). Otherwise a
        "return" trip of `_schedule_travel` from this minute, caused by the service event, without
        a facility and without validating the whole state.
        """


# Which step a toilet's completion leads to, for the table of rules/dispatch.rs. A patch of the
# step shows there, and the walk home then goes through it.
native.rules("dispatch").install_return_rule(DispatchPhase._schedule_return_to_initial_zone)

"""The dispatch phase: apply what is due, then what follows from the applied events."""

from __future__ import annotations

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.world import WorldState
from sheltersim.events.models import ScheduledEvent


class DispatchPhase:
    """Steps of TickPhase.DISPATCH_DUE_EVENTS, a mixin of OfflineSimulationController."""

    def _run_dispatch_phase(self, world: WorldState, minute: int) -> None:
        """Apply what is due at `minute` and what follows: native/core/src/rules/dispatch.rs.

        Each event due now, in queue order, unless it left the queue or another event of its ID
        took its place: `event_dispatcher`'s handler of its type (an EffectProgram's guard first;
        none is a FAILED record), its record, and the handler's follow-up records with the shared
        receipts among them. A handler of a class `events/handlers.py` installs runs in Rust; any
        other (a test's) through its `apply`. Then, for each APPLIED event: the open action it ends
        and a population exit; for `agent_travel_complete`, `learn_facility` of its person and
        facility when both are str, and `_record_staff_support_started` for a
        `staff_support_travel` trip; for `support_request_timeout`, `_record_request_interrupted`.
        `_refresh_space` once if a geometry or structure event applied, the staff trips on a route
        an applied geometry event closed, `admit_pending_returns`, `_sync_movement_after_events`,
        `admit_pending_arrivals`, and `_consider_social_contact_event` for each applied
        `information_contact_update`.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        before the first event raises KernelRefusal: a dispatcher of another class, a negative
        minute, an event queue or counters it cannot read. Past that, a refusal is a RuntimeError.
        """

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

    @kernel_only("dispatch", "sync_movement")
    def _sync_movement_after_events(self) -> None:
        """Finish the journeys of people who stopped moving, in ID order.

        A journey ends when its person left or is neither moving nor queued on a queue trip
        (`toilet_queue_position`, `resource_queue_position`, `toilet_queue_service`,
        `resource_queue_service`): `finish_movement`, and a walk to a reserved bed (`sleep_bed`)
        places the person at the bed's point of the overlay's reservation. Nothing without
        `movement_v1`.
        """

    @kernel_only("dispatch", "consider_contact")
    def _consider_social_contact_event(self, event: ScheduledEvent) -> None:
        """Ask the social policy about an applied `information_contact_update` (decision D5).

        Nothing without a social policy, for another type of event, or unless the sender and the
        recipient are two different people in the world. Sender and recipient of the same current
        household, one of them at or above `limits.stress_threshold`: `consider_cooperation`. A
        FALSE message the recipient has HEARD in `information_spread_v1`:
        `consider_information_reaction`.
        """

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

"""Deterministic toilet requests, FIFO queues, and capacity arbitration."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from sheltersim.core.kernels import kernel_only
from sheltersim.core.rng import KeyedRandom
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.facility_queues import (
    FACILITY_QUEUES_STATE_KEY,
    facility_has_independent_queues,
    point_for_queue,
    queue_ids_for_facility,
)
from sheltersim.domain.movement import (
    progress_physical_queue_head_to_service,
    progress_physical_queue_positions,
)
from sheltersim.domain.queueing import prepare_facility_queue, prepare_independent_facility_queue
from sheltersim.events.models import ScheduledEvent

TOILET_SERVICE_POLICY_VERSION = "toilet-service-uniform-v1"

if TYPE_CHECKING:
    from sheltersim.core.engine import TickPhase


@dataclass(frozen=True)
class ToiletRequestReceipt:
    """Stable request identity and queue position at admission time."""

    agent_id: str
    facility_id: str
    requested_at: int
    queue_task_id: str
    queue_position: int


class ToiletServiceManager:
    """Accept toilet requests and start queued services in FIFO order."""

    policy_version = TOILET_SERVICE_POLICY_VERSION

    def __init__(
        self,
        world: WorldState,
        *,
        min_service_minutes: int = 4,
        max_service_minutes: int = 8,
    ):
        if (
            not isinstance(min_service_minutes, int)
            or isinstance(min_service_minutes, bool)
            or not isinstance(max_service_minutes, int)
            or isinstance(max_service_minutes, bool)
        ):
            raise ValueError("service duration bounds must be integers")
        if min_service_minutes <= 0 or max_service_minutes < min_service_minutes:
            raise ValueError("service duration bounds must be positive and ordered")
        self.world = world
        self.min_service_minutes = min_service_minutes
        self.max_service_minutes = max_service_minutes

    @kernel_only("toilet", "request")
    def request(
        self,
        agent_id: str,
        facility_id: str,
        *,
        minute: int | None = None,
        validate_state: bool = True,
        queue_id: str | None = None,
    ) -> ToiletRequestReceipt:
        """Place one eligible Agent at the tail of a toilet Facility's FIFO queue.

        `minute`, when given, must be an int (ValueError "toilet request minute must be an
        integer") and the world's ("toilet request minute must match WorldState"). With
        `validate_state` the world is validated before and after. Then, in this order, ValueError
        for an unknown Agent ("unknown Agent 'A'"), a facility that is unknown or whose folded kind
        has no "toilet" ("'F' is not a toilet Facility"), an Agent who is not idle, resting or
        sleeping ("Agent 'A' cannot request a toilet while active") or is already queued or in
        service at any facility ("Agent 'A' already has an active toilet request"), and
        StateInvariantError for a queue task ID in use. The task is "toilet-queue-" and the first 24
        hex digits of the SHA-256 of the facility, the Agent and the minute joined by NUL (UTF-8).
        At a facility with independent queues, `queue_id` (or `choose_queue_id`'s) names the queue,
        whose signed point must be this facility's (ValueError "no accessible independent toilet
        service queue is available"); the request joins it and its "queued" transition is
        recorded. The Agent is queued, with the facility as target. Returns the receipt with the
        place in the queue.
        """

    def __call__(self, phase: TickPhase, world: WorldState, minute: int, rng: KeyedRandom) -> None:
        """Use as a tick phase callback; capacity is arbitrated after proposals."""
        if world is not self.world:
            raise ValueError("toilet service manager is bound to a different WorldState")
        if phase == "arbitrate_actions":
            self.arbitrate(minute, rng)

    def arbitrate(
        self,
        minute: int,
        rng: KeyedRandom,
        *,
        validate_state: bool = True,
    ) -> tuple[ScheduledEvent, ...]:
        """Start as many FIFO requests as operational capacity permits."""
        world = self.world
        if minute != world.minute_index:
            raise ValueError("facility arbitration minute must match WorldState")
        started: list[ScheduledEvent] = []
        for facility in self._facilities_to_serve():
            if facility_has_independent_queues(world, facility.id):
                started.extend(self._arbitrate_independent_facility(facility, minute, rng))
                continue
            prepare_facility_queue(world, facility.id, resource=False, minute=minute)
            while self._has_room(facility.id):
                if progress_physical_queue_positions(
                    world, facility.id, resource=False, minute=minute
                ):
                    break
                if progress_physical_queue_head_to_service(
                    world, facility.id, resource=False, minute=minute
                ):
                    break
                event_id = self._start_head(facility.id, minute, rng)
                if event_id is None:
                    break
                started.append(world.event_queue.find(event_id))

        if validate_state:
            world.validate()
        return tuple(started)

    @kernel_only("toilet", "has_room")
    def _has_room(self, facility_id: str) -> bool:
        """Whether someone is in the facility's shared queue and fewer are in service than its
        `physical_service_capacity` of its working units. Not for independent queues."""

    @kernel_only("toilet", "start_head")
    def _start_head(self, facility_id: str, minute: int, rng: KeyedRandom) -> str | None:
        """The head of the shared queue starts its service, if it can finish in the Run.

        A head without exactly one toilet queue task at the facility, or one queued after `minute`,
        is refused (KernelRefusal): the world's validation keeps these out. The service lasts
        `min_service_minutes` + int(draw * span), at most `max_service_minutes`, with the draw of
        `rng` at ("toilet-service", facility, agent, "minute", minute, policy_version, "duration");
        a draw outside 0..1 is a ValueError ("keyed RNG draw must be between zero and one"). A
        service that would end at or past the Run's end starts nothing: None, the queue in order.
        Otherwise the head leaves the queue for the facility's active list, the queue task becomes a
        toilet_service task "toilet-service-<sequence>" with its toilet_service_complete event
        "toilet-complete-<sequence>" (its provenance: the duration, the wait, the key and the draw),
        the person is in service, and the wait is counted in toilet_wait_count,
        toilet_wait_total_minutes and toilet_wait_max_minutes. Returns the event's ID.
        """

    @kernel_only("toilet", "facilities_to_serve")
    def _facilities_to_serve(self) -> list[Any]:
        """The toilet facilities (kind folded) with someone in their shared queue or one of their
        independent queues, in ID order: one with nobody queued does nothing in `arbitrate`."""

    def _arbitrate_independent_facility(
        self, facility: object, minute: int, rng: KeyedRandom
    ) -> list[ScheduledEvent]:
        world = self.world
        queue_state = world.model_state[FACILITY_QUEUES_STATE_KEY]
        started: list[ScheduledEvent] = []
        for queue_id in queue_ids_for_facility(world, facility.id):
            prepare_independent_facility_queue(world, queue_id, minute=minute, resource=False)
            queue = queue_state["queues"][queue_id]
            if point_for_queue(world, queue_id) is None:
                raise StateInvariantError(f"service queue {queue_id!r} lost its signed point")
            while queue["queued_request_ids"]:
                if progress_physical_queue_positions(
                    world, facility.id, resource=False, minute=minute, queue_id=queue_id
                ):
                    break
                event_id = self._serve_independent_head(facility.id, queue_id, minute, rng)
                if event_id is None:
                    break
                started.append(world.event_queue.find(event_id))
        return started

    @kernel_only("toilet", "serve_independent_head")
    def _serve_independent_head(
        self, facility_id: str, queue_id: str, minute: int, rng: KeyedRandom
    ) -> str | None:
        """The head of one independent queue starts its service: the event's ID, or None to stop.

        A full point (its `physical_service_capacity`) or facility (its capacity and working units)
        marks a "capacity" stall; a head still walking to the point stops the queue. A queue that
        lost its toilet queue task, a wait in the future or a person missing from the facility's
        list is refused (KernelRefusal): the world's validation keeps these out. The duration is
        drawn as in `_start_head`, with the queue ID last in the key; a service past the Run's end
        marks a "run_end" stall. A point that asks staff skills takes the first free, available
        staff member of the facility's zone with them, by ID, else a "staff" stall. Then the person
        leaves the facility's queue list for its active list, the task and its event are made as
        in `_start_head` (with the staff member and the queue ID), the staff member works it, the
        queue's request starts with its "service_started" transition, and the wait is counted.
        """

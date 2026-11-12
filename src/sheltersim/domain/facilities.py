"""Deterministic toilet requests, FIFO queues, and capacity arbitration."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING

from sheltersim.core.kernels import kernel_only
from sheltersim.core.rng import KeyedRandom
from sheltersim.core.world import WorldState
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

    @kernel_only("toilet", "arbitrate")
    def arbitrate(
        self,
        minute: int,
        rng: KeyedRandom,
        *,
        validate_state: bool = True,
    ) -> tuple[ScheduledEvent, ...]:
        """Start as many FIFO requests as operational capacity permits; the started events.

        `minute` must equal `world.minute_index` (ValueError "facility arbitration minute must
        match WorldState"). Then each toilet facility (kind folded) with someone in its shared queue
        or one of its independent queues, in ID order:

        - A facility with independent queues serves each queue in turn: the queue is prepared, its
          signed point must still exist (StateInvariantError "service queue 'Q' lost its signed
          point"), and while it has someone queued its positions move on, then its head starts its
          service, until either stops the queue. A full point (its `physical_service_capacity`) or
          facility (its capacity and working units) marks a "capacity" stall; a head still walking
          to the point stops the queue. A point that asks staff skills takes the first free,
          available staff member of the facility's zone with them, by ID, else a "staff" stall.
          The queue's request starts with its "service_started" transition.
        - Any other facility's shared queue is prepared, and while someone is queued and fewer are
          in service than its `physical_service_capacity` of its working units, the positions and
          then the head move on, and the head starts its service.

        A head without exactly one toilet queue task at the facility, or one queued after `minute`,
        is refused (KernelRefusal): the world's validation keeps these out. The service lasts
        `min_service_minutes` + int(draw * span), at most `max_service_minutes`, with the draw of
        `rng` at ("toilet-service", facility, agent, "minute", minute, policy_version, "duration"),
        the queue ID last for an independent queue; a draw outside 0..1 is a ValueError ("keyed RNG
        draw must be between zero and one"). A service that would end at or past the Run's end
        starts nothing (an independent queue marks a "run_end" stall). Otherwise the head leaves
        the queue for the facility's active list, the queue task becomes a toilet_service task
        "toilet-service-<sequence>" with its toilet_service_complete event
        "toilet-complete-<sequence>" (its provenance: the duration, the wait, the key and the draw),
        the person is in service, and the wait is counted in toilet_wait_count,
        toilet_wait_total_minutes and toilet_wait_max_minutes. With `validate_state` the world is
        validated after.

        The loop is the core's (native/core/src/rules/toilet.rs, lane P9-3b of #152); a step's
        refusal raises KernelRefusal with the step's name ("toilet.start_head refused: ...").
        """

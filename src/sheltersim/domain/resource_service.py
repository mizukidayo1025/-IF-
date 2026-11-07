"""Deterministic staffed water and meal service for ResourceLedger v1."""

from __future__ import annotations

from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from sheltersim import native
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.facility_queues import (
    FACILITY_QUEUES_STATE_KEY,
    facility_has_independent_queues,
    point_for_queue,
    queue_ids_for_facility,
)
from sheltersim.domain.movement import progress_physical_queue_positions
from sheltersim.domain.queueing import prepare_facility_queue, prepare_independent_facility_queue
from sheltersim.domain.resources import RESOURCE_LEDGER_KEY
from sheltersim.events.models import ScheduledEvent

if TYPE_CHECKING:
    from sheltersim.core.rng import KeyedRandom


SERVICE_STATE_KEY = "resource_service_v1"


@dataclass(frozen=True)
class ResourceRequestReceipt:
    """Stable queue receipt for one water or meal service request."""

    agent_id: str
    facility_id: str
    resource_id: str
    requested_at: int
    queue_task_id: str
    queue_position: int


class ResourceServiceManager:
    """Manage FIFO requests, one-staff service starts, and ledger-backed completion."""

    policy_version = "resource-service-v1"

    def __init__(self, world: WorldState) -> None:
        if world.schema_version != 6 or "resource_service" not in world.model_configs:
            raise ValueError("ResourceServiceManager requires an opted-in WorldState v6")
        self.world = world
        self.config = dict(world.model_configs["resource_service"])
        if (
            SERVICE_STATE_KEY not in world.model_state
            or RESOURCE_LEDGER_KEY not in world.model_state
        ):
            raise ValueError("WorldState v6 requires resource service and ledger state")

    @kernel_only("service", "request")
    def request(
        self,
        agent_id: str,
        facility_id: str,
        *,
        minute: int | None = None,
        validate_state: bool = True,
        queue_id: str | None = None,
    ) -> ResourceRequestReceipt:
        """Put an eligible Agent at the tail of the correct resource FIFO queue.

        ValueError, in this order: a minute that is not an int or not the world's; (after
        `world.validate()` when `validate_state`) an unknown Agent, a facility that is not a water
        station or meal distribution, an Agent in another zone, an Agent not idle, resting or
        sleeping, an Agent with an exclusive task or in any facility's queue or service. The next
        free "resource-service-task-N" (`id_counters["resource_task"]`, skipping active tasks,
        used reservation IDs and pending reserve events) is the queue task. A facility with
        independent queues takes the given queue_id or `choose_queue_id`'s, and enqueues there
        ("no accessible independent resource service queue is available" when there is none, after
        the counter moved); else the facility's own list. The Agent is QUEUED with the facility as
        target, the "queued" outcome is recorded, and the receipt gives the place in the queue.
        """

    def arbitrate(
        self,
        minute: int,
        rng: KeyedRandom | None = None,
        *,
        validate_state: bool = True,
    ) -> tuple[ScheduledEvent, ...]:
        """Start available FIFO heads in stable facility and staff ID order."""
        del rng  # Service duration is a versioned fixed assumption, not a random draw.
        world = self.world
        if minute != world.minute_index:
            raise ValueError("resource service arbitration minute must match WorldState")
        started: list[ScheduledEvent] = []
        for facility, resource_id in self._facilities_to_serve():
            if facility_has_independent_queues(world, facility.id):
                started.extend(self._arbitrate_independent_facility(facility, resource_id, minute))
                continue
            prepare_facility_queue(world, facility.id, resource=True, minute=minute)
            while facility.resource_queue_agent_ids:
                if progress_physical_queue_positions(
                    world, facility.id, resource=True, minute=minute
                ):
                    break
                event_id = self._serve_head(facility.id, resource_id, minute)
                if event_id is None:
                    break
                started.append(world.event_queue.find(event_id))
        if validate_state:
            world.validate()
        return tuple(started)

    @kernel_only("service", "facilities_to_serve")
    def _facilities_to_serve(self) -> list[tuple[Any, str]]:
        """The resource facilities in ID order, with the resource of each.

        Only those with someone in their own list or in one of their independent queues: an
        empty queue starts nothing.
        """

    @kernel_only("service", "serve_head")
    def _serve_head(self, facility_id: str, resource_id: str, minute: int) -> str | None:
        """The queue's head starts its service, if nothing holds it: the event's ID, or None.

        StateInvariantError when the head has not exactly one queue task, or its ID is a ledger
        reservation's or a pending reserve event's. A stall ("water_outage", "capacity", "staff",
        "stock", "run_end") is recorded once per head and reason, and counted. Else, unless the head
        still walks to the service point, the first free qualified staff member in ID order serves
        it for `service_minutes`: the completion event is scheduled ("resource event identity ... is
        not unique" if taken), the stock is reserved under the task's ID (its audit event
        "resource-reserve-N" is recorded applied), and the wait is counted.
        """

    def _arbitrate_independent_facility(
        self, facility: object, resource_id: str, minute: int
    ) -> list[ScheduledEvent]:
        world = self.world
        queue_state = world.model_state[FACILITY_QUEUES_STATE_KEY]
        started: list[ScheduledEvent] = []
        # Each queue's head may wait for its own reason; the facility keeps one of them.
        stalls: list[tuple[str, str]] = []
        previous = world.model_state[SERVICE_STATE_KEY]["stall_state"].get(facility.id)
        for queue_id in queue_ids_for_facility(world, facility.id):
            prepare_independent_facility_queue(world, queue_id, minute=minute, resource=True)
            queue = queue_state["queues"][queue_id]
            if point_for_queue(world, queue_id) is None:
                raise StateInvariantError(f"resource queue {queue_id!r} lost its signed point")
            while queue["queued_request_ids"]:
                if progress_physical_queue_positions(
                    world, facility.id, resource=True, minute=minute, queue_id=queue_id
                ):
                    break
                event_id, stall = self._serve_independent_head(
                    facility.id, resource_id, queue_id, minute
                )
                if stall is not None:
                    stalls.append(stall)
                if event_id is None:
                    break
                started.append(world.event_queue.find(event_id))
        self._record_independent_queue_stall(facility.id, stalls, previous)
        return started

    @kernel_only("service", "serve_independent_head")
    def _serve_independent_head(
        self, facility_id: str, resource_id: str, queue_id: str, minute: int
    ) -> tuple[str | None, tuple[str, str] | None]:
        """The head of one independent queue starts its service, if nothing holds it.

        The started event's ID or None to stop the queue, and why the head waits, if it does
        (the queue's stall is marked: "outage", "capacity", "staff", "stock", "run_end"). The
        point's capacity and required staff skills apply too. As `_serve_head` otherwise, and
        the queue request starts its service with a "service_started" transition.
        """

    @kernel_only("service", "record_independent_stall")
    def _record_independent_queue_stall(
        self, facility_id: str, stalls: list[tuple[str, str]], previous: dict[str, str] | None
    ) -> None:
        """Keep a still-waiting head's record, else record the first stalled queue's head.

        Recording every queue's head in turn would log a new stall each minute while
        two queues both wait, and count each wait reason again every minute (#118).
        """

    @kernel_only("service", "complete")
    def complete(self, event: ScheduledEvent) -> None:
        """Consume one completed reservation, relieve its need, and release staff/slot.

        EventApplicationError, in this order: no str task_id; no active task (EventSuppressed when
        the task was cancelled); an event that is not this service's completion, or does not match
        its task; a reservation, slot, staff or quantity that is not the service's full one; and
        "resource consume rejected: ..." when the ledger turns the consume down. Then hydration or
        hunger falls by the configured relief (not below 0), the queue request completes, the
        staff member is free, the Agent IDLE, and the completed counts go up.
        """

    @kernel_only("handlers", "cancel")
    def cancel(self, event: ScheduledEvent) -> None:
        """Cancel queued or active service; active inventory reservations are released once.

        EventApplicationError, in this order: no non-empty str task_id; a payload other than
        task_id and a reason of operator_cancelled, facility_stopped or staff_unavailable;
        (EventSuppressed for a task already cancelled) no active resource task; a queued Agent
        missing from its queue; an active task at a non-resource facility, without its slot, or
        whose staff, reservation or service ownership differs; and "resource reservation release
        rejected: ..." when the ledger turns the release down. The queue request ends interrupted
        (staff_unavailable) or cancelled, the Agent is IDLE, and the task joins
        cancelled_task_ids.
        """

    @kernel_only("handlers", "interrupt_for_staff")
    def interrupt_for_staff(self, staff_id: str, *, causation_event: ScheduledEvent) -> None:
        """Cancel an interrupted service and put its Agent at the FIFO head.

        Nothing when the staff member serves no water or meal. EventApplicationError when they
        serve several, when the task's ownership is incomplete or inconsistent with the Agent,
        facility and staff, when its facility is not a resource one, or when its reservation is
        not the service's. A "resource-service-cancel-N" event (staff_unavailable) is cancelled as
        `cancel` does and recorded; the Agent gets a new queue task at its first queued minute,
        first in the facility's list (and its independent queue, with a "requeued" transition),
        and a "queued" outcome with detail "service_requeued_after_staff_unavailable".
        """

    @staticmethod
    def resource_for_facility(kind: str) -> str | None:
        if kind == "water_station":
            return "water_ml"
        if kind == "meal_distribution":
            return "food_meal_units"
        return None

    @kernel_only("service", "record_request_outcome")
    def record_request_outcome(
        self,
        *,
        agent_id: str,
        action: str,
        outcome: str,
        reason: str,
        facility_id: str | None,
        origin_zone_id: str,
        detail: str | None = None,
    ) -> None:
        """Persist why an attempted resource action queued, moved, or was rejected.

        Appends {minute, agent_id, action, outcome, reason, facility_id, origin_zone_id, detail}
        to the service's request_history.
        """


register_rust_validator(SERVICE_STATE_KEY, native.state("resource_service").validate)

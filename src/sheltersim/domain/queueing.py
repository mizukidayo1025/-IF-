"""FIFO reservation order, need triage, and recorded queue exit/retry transitions.

Both rules are `native/src/rules/queues.rs`'s kernels (`kernel_only`). The queue policy is
`policies/queueing.py`'s `create_queue_policy`: the need-weighted policy orders a shared queue and
ends overlong waits; the strict FIFO policy does neither.
"""

from __future__ import annotations

from sheltersim.core.kernels import kernel_only
from sheltersim.core.world import WorldState


@kernel_only("queues", "prepare_facility_queue")
def prepare_facility_queue(
    world: WorldState, facility_id: str, *, resource: bool, minute: int
) -> None:
    """Apply the registered queue policy to the existing reservation list.

    Nothing for an unknown facility, an empty list (`resource_queue_agent_ids` when resource, else
    `queue_agent_ids`), or while a `movement_v1` journey walks to a place of this facility's queue
    or service. Otherwise the list keeps only those with a queue task here ("resource_service_queue"
    or "toilet_queue"), ordered by the policy's priority (highest first), then the task's start,
    then the place held; a resource stall record whose Agent no longer heads the facility's queue
    is dropped. Then each, in that order, whom the policy's `should_abandon` lets go leaves the
    queue: an applied `facility_queue_abandon` event (detail "Agent left a prolonged low-urgency
    queue and will reconsider after a bounded delay"), the queue task removed, the next decision
    after the policy's retry delay, `facility_queue_abandon_count` + 1, and a walk back to the
    zone's anchor ("queue_abandon", an `agent_travel_complete` event) when there is a path to it.
    Someone whose path is blocked, or whose walk would end after the Run, stays.

    The facility's kind names the service: "toilet" in kind.casefold(), "water_station",
    "meal_distribution"; another raises ValueError("unsupported queue service facility kind
    <repr>"). A facility_queues config the policy turns down raises the policy's ValueError.
    """


@kernel_only("queues", "prepare_independent_facility_queue")
def prepare_independent_facility_queue(
    world: WorldState, queue_id: str, *, minute: int, resource: bool
) -> None:
    """Apply only abandonment rules to an explicitly independent FIFO.

    The registered priority policy may end an overlong wait, but it does not reorder
    requests inside a queue whose contract promises FIFO service.

    Nothing without `facility_queues_v1` or the queue, while a journey walks to a place of this
    queue, or with nobody queued. Each queued request, in order, whose queue task exists and whom
    `should_abandon` lets go leaves as in prepare_facility_queue, and its request ends
    ("abandoned", with a `facility_queue_transition` record).
    """

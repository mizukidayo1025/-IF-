"""Independent checkpointed FIFO state for explicitly distinct service queues."""

from __future__ import annotations

from typing import Any

from sheltersim import native
from sheltersim.contracts.extensions import FacilityServicePoint
from sheltersim.core.kernels import kernel_first, kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import WorldState
from sheltersim.domain.geometry_overlay import parsed_facility_geometry

FACILITY_QUEUES_STATE_KEY = "facility_queues_v1"
FACILITY_QUEUES_POLICY_VERSION = "independent-facility-queues-v1"
FACILITY_QUEUES_LEGACY_MODE_KEY = "facility_queues_legacy_mode_v1"
FACILITY_QUEUES_LEGACY_MODE = {
    "schema_version": 1,
    "mode": "pre_independent_queue_checkpoint",
    "policy_version": "facility_fifo_legacy-v1",
}
MAX_QUEUE_REQUESTS = 100_000


def service_points(world: WorldState) -> tuple[FacilityServicePoint, ...]:
    raw = world.model_configs.get("facility_geometry")
    if not isinstance(raw, dict):
        return ()
    return parsed_facility_geometry(raw).service_points


@kernel_only("facility_queues", "independent_queues_required")
def independent_queues_required(configs: dict[str, Any]) -> bool:
    """Opt in only when queue IDs change legacy one-FIFO-per-facility semantics.

    True when the signed `facility_geometry` config has a service point whose queue ID is not its
    facility's ID, or a facility with two points; False without a config dict. A config that
    `parsed_facility_geometry` turns down raises its error.
    """


@kernel_only("facility_queues", "is_legacy_queue_checkpoint")
def is_legacy_queue_checkpoint(world: WorldState) -> bool:
    """A checkpoint from before independent queues, migrated with the legacy marker.

    True when independent queues are required, `facility_queues_v1` is absent (or None), and
    `facility_queues_legacy_mode_v1` equals FACILITY_QUEUES_LEGACY_MODE.
    """


@kernel_only("facility_queues", "create_facility_queue_state")
def create_facility_queue_state(world: WorldState) -> dict[str, Any]:
    """The empty `facility_queues_v1` state: one queue per signed service point, by queue ID.

    Each queue holds its facility and point IDs, empty `queued_request_ids` and
    `active_request_ids`, and no `last_stall`. ValueError("facility_queues_v1 requires
    independent explicit service points") when independent queues are not required.
    """


def validate_facility_queue_state(world: WorldState) -> None:
    state = world.model_state.get(FACILITY_QUEUES_STATE_KEY)
    required = independent_queues_required(world.model_configs)
    if state is None:
        if required and not is_legacy_queue_checkpoint(world):
            raise ValueError("independent service points require facility_queues_v1 state")
        return
    if FACILITY_QUEUES_LEGACY_MODE_KEY in world.model_state:
        raise ValueError("legacy facility queue mode cannot be mixed with independent queue state")
    if not required:
        raise ValueError("facility_queues_v1 has no matching independent service-point config")
    if (
        not isinstance(state, dict)
        or set(state) != {"schema_version", "policy_version", "queues", "requests"}
        or state.get("schema_version") != 1
        or state.get("policy_version") != FACILITY_QUEUES_POLICY_VERSION
        or not isinstance(state.get("queues"), dict)
        or not isinstance(state.get("requests"), dict)
        or len(state["requests"]) > MAX_QUEUE_REQUESTS
    ):
        raise ValueError("facility_queues_v1 has an invalid exact schema")
    expected = {point.queue_id: point for point in service_points(world)}
    if set(state["queues"]) != set(expected):
        raise ValueError("facility_queues_v1 queue IDs do not match the signed service points")
    active_agents: set[str] = set()
    for queue_id, row in state["queues"].items():
        point = expected[queue_id]
        if (
            not isinstance(row, dict)
            or set(row)
            != {
                "facility_id",
                "service_point_id",
                "queued_request_ids",
                "active_request_ids",
                "last_stall",
            }
            or row["facility_id"] != point.facility_id
            or row["service_point_id"] != point.id
            or not isinstance(row["queued_request_ids"], list)
            or not isinstance(row["active_request_ids"], list)
            or len(row["queued_request_ids"]) != len(set(row["queued_request_ids"]))
            or len(row["active_request_ids"]) != len(set(row["active_request_ids"]))
            or set(row["queued_request_ids"]) & set(row["active_request_ids"])
        ):
            raise ValueError(f"facility queue {queue_id!r} is invalid")
        if len(row["active_request_ids"]) > point.service_capacity:
            raise ValueError(f"facility queue {queue_id!r} exceeds its signed concurrent capacity")
        stall = row["last_stall"]
        if stall is not None and (
            not isinstance(stall, dict)
            or set(stall) != {"minute", "request_id", "reason"}
            or type(stall["minute"]) is not int
            or stall["minute"] < 0
            or not isinstance(stall["request_id"], str)
            or stall["reason"] not in {"capacity", "staff", "stock", "outage", "route", "run_end"}
        ):
            raise ValueError(f"facility queue {queue_id!r} stall state is invalid")
        for request_id in (*row["queued_request_ids"], *row["active_request_ids"]):
            request = state["requests"].get(request_id)
            if (
                not isinstance(request, dict)
                or request.get("request_id") != request_id
                or request.get("queue_id") != queue_id
                or request.get("facility_id") != point.facility_id
                or request.get("service_point_id") != point.id
                or request.get("agent_id") not in world.agents
                or request.get("service_kind") not in {"toilet", "water", "meal"}
                or type(request.get("requested_minute")) is not int
                or not 0 <= request["requested_minute"] <= world.minute_index
                or request.get("status")
                != ("queued" if request_id in row["queued_request_ids"] else "active")
                or not isinstance(request.get("queue_task_id"), str)
                or not isinstance(request.get("active_task_id"), str)
            ):
                raise ValueError(f"facility queue request {request_id!r} is invalid")
            agent_id = request["agent_id"]
            if agent_id in active_agents:
                raise ValueError("an Agent cannot occupy two independent service queues")
            active_agents.add(agent_id)
            facility = world.facilities[point.facility_id]
            agent = world.agents[agent_id]
            expected_kind = (
                "water_station"
                if request["service_kind"] == "water"
                else "meal_distribution"
                if request["service_kind"] == "meal"
                else None
            )
            if expected_kind is not None and facility.kind != expected_kind:
                raise ValueError("facility queue service kind does not match its resource facility")
            if request_id in row["queued_request_ids"]:
                task = world.active_tasks.get(request["queue_task_id"])
                if (
                    task is None
                    or task.kind not in {"toilet_queue", "resource_service_queue"}
                    or task.agent_id != agent_id
                    or task.resource_id != point.facility_id
                    or agent.activity.value != "queued"
                    or agent.target_id != point.facility_id
                ):
                    raise ValueError("queued service request has no matching task and Agent state")
                mirror = (
                    facility.resource_queue_agent_ids
                    if request["service_kind"] in {"water", "meal"}
                    else facility.queue_agent_ids
                )
                if agent_id not in mirror:
                    raise ValueError(
                        "independent queue request is missing from the facility projection"
                    )
            else:
                task = world.active_tasks.get(request["active_task_id"])
                if (
                    task is None
                    or task.agent_id != agent_id
                    or task.resource_id != point.facility_id
                    or task.kind
                    not in {"toilet_service", "resource_service_water", "resource_service_food"}
                    or agent.activity.value != "in_service"
                    or agent.target_id != point.facility_id
                ):
                    raise ValueError(
                        "active service request has no matching service task and Agent state"
                    )
                if request["service_kind"] == "toilet" and (
                    point.access.required_staff_skills or task.staff_id is not None
                ):
                    staff_member = world.staff.get(task.staff_id or "")
                    if (
                        not point.access.required_staff_skills
                        or staff_member is None
                        or not set(point.access.required_staff_skills) <= set(staff_member.skills)
                        or staff_member.zone_id != world.facilities[point.facility_id].zone_id
                        or staff_member.active_task_id != task.id
                        or staff_member.work_queue != [task.id]
                        or staff_member.busy_until != task.due_minute
                    ):
                        raise ValueError(
                            "staff-required toilet service has no matching skilled staff "
                            "reservation"
                        )
                mirror = (
                    facility.resource_active_agent_ids
                    if request["service_kind"] in {"water", "meal"}
                    else facility.active_agent_ids
                )
                if agent_id not in mirror:
                    raise ValueError(
                        "active independent service is missing from the facility projection"
                    )
    if set(state["requests"]) != {
        request_id
        for row in state["queues"].values()
        for request_id in (*row["queued_request_ids"], *row["active_request_ids"])
    }:
        raise ValueError("facility_queues_v1 contains an unassigned or terminal request")
    movement = world.model_state.get("movement_v1", {})
    journeys = movement.get("journeys", {}) if isinstance(movement, dict) else {}
    for agent_id, journey in journeys.items():
        queue_id = journey.get("queue_id")
        if queue_id is None:
            continue
        request = request_for_agent(world, agent_id, queue_id=queue_id)
        if request is None or request["status"] != "queued":
            raise ValueError("queue movement journey has no matching queued request")


@kernel_only("facility_queues", "choose_queue_id")
def choose_queue_id(world: WorldState, facility_id: str, agent_id: str) -> str | None:
    """Choose an accessible queue by current load and stable queue ID.

    Among the facility's signed points, a restricted point counts only for an Agent with all its
    `required_access_tags` (the person's `accessibility_needs` and mobility). The queue with the
    fewest queued and active requests wins, then the smaller queue ID. None without the state or
    without such a point.
    """


def point_for_queue(world: WorldState, queue_id: str) -> FacilityServicePoint | None:
    return next((point for point in service_points(world) if point.queue_id == queue_id), None)


@kernel_only("facility_queues", "queue_ids_for_facility")
def queue_ids_for_facility(world: WorldState, facility_id: str) -> tuple[str, ...]:
    """The queue IDs of the facility's signed service points, sorted."""


@kernel_only("facility_queues", "facility_has_independent_queues")
def facility_has_independent_queues(world: WorldState, facility_id: str) -> bool:
    """Whether `facility_queues_v1` holds a queue of this facility."""


@kernel_first("facility_queues", "queue_head_agent_ids")
def queue_head_agent_ids(world: WorldState, facility_id: str) -> set[str]:
    """The Agents first in line at a facility: one per independent queue, else the shared head."""
    state = world.model_state.get(FACILITY_QUEUES_STATE_KEY)
    if facility_has_independent_queues(world, facility_id):
        return {
            state["requests"][row["queued_request_ids"][0]]["agent_id"]
            for row in state["queues"].values()
            if row["facility_id"] == facility_id and row["queued_request_ids"]
        }
    return set(world.facilities[facility_id].resource_queue_agent_ids[:1])


@kernel_only("facility_queues", "request_for_task")
def request_for_task(world: WorldState, task_id: str) -> dict[str, Any] | None:
    """The live request whose ID is task_id, else the one whose `active_task_id` is task_id.

    The state's own record, or None.
    """


@kernel_only("facility_queues", "request_for_agent")
def request_for_agent(
    world: WorldState, agent_id: str, *, queue_id: str | None = None
) -> dict[str, Any] | None:
    """Return the live queue reservation owned by an Agent.

    The first request of the Agent, in the queue `queue_id` when given: the state's own record,
    or None.
    """


@kernel_only("facility_queues", "queue_agent_ids")
def queue_agent_ids(world: WorldState, queue_id: str) -> list[str]:
    """The Agents of the queue's queued requests, in order; [] for an unknown queue."""


@kernel_only("facility_queues", "enqueue")
def enqueue(
    world: WorldState,
    *,
    queue_id: str,
    request_id: str,
    task_id: str,
    agent_id: str,
    service_kind: str,
    requested_minute: int,
) -> None:
    """Add a queued request at the end of the queue.

    The record names the queue's signed point and facility, with `queue_task_id` and
    `active_task_id` both task_id. ValueError("service queue is not installed or request ID is
    already in use") when the state is missing, the queue has no signed point, or the request ID is
    taken.
    """


@kernel_only("facility_queues", "start_service")
def start_service(world: WorldState, request_id: str, active_task_id: str) -> dict[str, Any]:
    """Move a queued request to the queue's active list and return its record.

    The request becomes "active" with active_task_id, and the queue's `last_stall` is cleared.
    """


@kernel_only("facility_queues", "finish_service")
def finish_service(world: WorldState, task_id: str, *, status: str) -> dict[str, Any] | None:
    """End the request request_for_task finds: a copy of it with `status`, or None.

    The request leaves its queue's list and the state, and the queue's `last_stall` is cleared.
    """


@kernel_only("facility_queues", "mark_stall")
def mark_stall(world: WorldState, queue_id: str, request_id: str, reason: str) -> None:
    """Record why the queue's head waits: `last_stall` with the minute, request and reason.

    Nothing for an unknown queue or without the state.
    """


@kernel_only("facility_queues", "record_queue_transition")
def record_queue_transition(
    world: WorldState,
    request: dict[str, Any],
    *,
    transition: str,
    task_id: str,
    wait_minutes: int | None = None,
    causation_id: str | None = None,
) -> None:
    """Persist typed queue evidence into EventLog without inventing a scheduled event.

    Takes the next event sequence and appends an applied `facility_queue_transition` record,
    `facility-queue-transition-<sequence>`, whose payload FacilityQueueTransitionReceipt checks:
    the request's IDs, the transition, the minute, its place in the queue (1-based, or None when
    not queued), the point's concurrent capacity, the wait and the cause.
    """


register_rust_validator(FACILITY_QUEUES_STATE_KEY, native.rules("facility_queues").validate)

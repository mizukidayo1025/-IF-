"""Deterministic staff assignment and support-request lifecycle."""

from __future__ import annotations

import hashlib
from dataclasses import dataclass
from typing import Any, Callable

from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import read_only_part
from sheltersim.core.world import WorldState
from sheltersim.domain.care_receipts import care_receipt_for_request
from sheltersim.domain.models import (
    Activity,
    SupportRequestState,
    SupportRequestStatus,
)
from sheltersim.domain.operational import staff_is_available
from sheltersim.domain.staff_dynamics import (
    staff_dynamics_is_available,
    staff_dynamics_on_shift,
)
from sheltersim.events.models import (
    EventKind,
    ScheduledEvent,
)
from sheltersim.policies.space import ZoneRoute

_CARE_SKILL_ALTERNATIVES = {
    "emotional_support": {"support"},
    "medical_assessment": {"medical", "nursing"},
    "medication_support": {"medication", "nursing"},
    "caregiver_assistance": {"caregiver", "support"},
    "dependent_assistance": {"caregiver", "support", "nursing"},
    "privacy_accommodation": {"privacy", "support"},
    "mobility_assistance": {"mobility", "mobility_assistance", "support"},
    "hygiene_support": {"hygiene", "support"},
    "language_access": {"interpreter", "language_access"},
}
STAFF_SUPPORT_POLICY_VERSION = "staff-support-fifo-aging-v2"
STAFF_SUPPORT_SOURCE_ID = "ASM-STAFF-SUPPORT-LIFECYCLE-V1"


@dataclass(frozen=True)
class SupportRequestReceipt:
    """Stable identity and status for an accepted support request."""

    request_id: str
    agent_id: str
    requested_minute: int


class StaffService:
    """Assign pending support requests one at a time to skilled available staff."""

    policy_version = STAFF_SUPPORT_POLICY_VERSION

    def __init__(
        self,
        world: WorldState,
        *,
        task_duration_minutes: int = 10,
        unresolved_after_minutes: int = 90,
        cooldown_minutes: int = 30,
        fairness_after_minutes: int | None = None,
        route_resolver: Callable[[str, str, str], ZoneRoute | None] | None = None,
        travel_scheduler: Callable[[str, ZoneRoute, str, str, int], bool] | None = None,
    ) -> None:
        if fairness_after_minutes is None:
            fairness_after_minutes = default_support_fairness_minutes(unresolved_after_minutes)
        for name, value in (
            ("task_duration_minutes", task_duration_minutes),
            ("unresolved_after_minutes", unresolved_after_minutes),
            ("cooldown_minutes", cooldown_minutes),
            ("fairness_after_minutes", fairness_after_minutes),
        ):
            if not isinstance(value, int) or isinstance(value, bool) or value < 0:
                raise ValueError(f"{name} must be a non-negative integer")
        if task_duration_minutes == 0 or unresolved_after_minutes == 0:
            raise ValueError("task duration and unresolved timeout must be positive")
        if fairness_after_minutes == 0 or fairness_after_minutes > unresolved_after_minutes:
            raise ValueError("fairness promotion must occur before or at the request timeout")
        self.world = world
        self.task_duration_minutes = task_duration_minutes
        self.unresolved_after_minutes = unresolved_after_minutes
        self.cooldown_minutes = cooldown_minutes
        self.fairness_after_minutes = fairness_after_minutes
        self.route_resolver = route_resolver
        self.travel_scheduler = travel_scheduler

    def request(
        self,
        agent_id: str,
        *,
        minute: int | None = None,
        validate_state: bool = True,
    ) -> SupportRequestReceipt:
        """Open exactly one request per Agent and schedule its deterministic timeout."""
        world = self.world
        request_minute = world.minute_index if minute is None else minute
        if request_minute != world.minute_index:
            raise ValueError("support request minute must match WorldState")
        if validate_state:
            world.validate()
        agent = world.agents.get(agent_id)
        if agent is None:
            raise ValueError(f"unknown Agent {agent_id!r}")
        if request_minute < agent.support_cooldown_until:
            raise ValueError(f"Agent {agent_id!r} is in support-request cooldown")
        if agent.support_request_id is not None:
            raise ValueError(f"Agent {agent_id!r} already has an active support request")
        if agent.activity not in (Activity.IDLE, Activity.RESTING, Activity.SLEEPING):
            raise ValueError(f"Agent {agent_id!r} cannot request support while active")

        digest = hashlib.sha256(f"{agent_id}\0{request_minute}".encode()).hexdigest()[:24]
        request_id = f"support-{digest}"
        if request_id in world.support_requests:
            raise ValueError(f"support request {request_id!r} already exists")
        timeout_minute = min(
            request_minute + self.unresolved_after_minutes,
            world.duration_minutes - 1,
        )
        request = SupportRequestState(
            id=request_id,
            agent_id=agent_id,
            requested_minute=request_minute,
            timeout_minute=timeout_minute,
        )
        world.support_requests[request_id] = request
        if timeout_minute == request_minute:
            request.status = SupportRequestStatus.UNRESOLVED
            request.resolved_minute = request_minute
            request.cooldown_until = request_minute + self.cooldown_minutes
            agent.support_cooldown_until = request.cooldown_until
            world.metric_state["support_unresolved_count"] = (
                world.metric_state.get("support_unresolved_count", 0.0) + 1.0
            )
        else:
            agent.activity = Activity.SEEKING_HELP
            agent.target_id = None
            agent.support_request_id = request_id
            sequence = world.allocate_event_sequence()
            world.schedule_event(
                ScheduledEvent(
                    event_id=f"support-timeout-{sequence}",
                    kind=EventKind.COMPLETION,
                    due_minute=timeout_minute,
                    priority=20,
                    sequence=sequence,
                    event_type="support_request_timeout",
                    payload={"request_id": request_id, "agent_id": agent_id},
                    source_id=request_id,
                    provenance={
                        "source_type": "assumption",
                        "source_id": STAFF_SUPPORT_SOURCE_ID,
                        "policy_version": self.policy_version,
                        "timeout_minutes": self.unresolved_after_minutes,
                    },
                )
            )
        if validate_state:
            world.validate()
        return SupportRequestReceipt(request_id, agent_id, request_minute)

    @kernel_only("staff", "assign_pending")
    def assign_pending(self, minute: int, *, validate_state: bool = True) -> tuple[str, ...]:
        """Start FIFO requests on idle staff whose skills include ``support``.

        Returns the IDs of the requests it assigned. ValueError "staff assignment minute must
        match WorldState" first, for another minute than the world's. Pending requests go urgent
        (an LLM priority above 0) or aged past `fairness_after_minutes` first, then by request
        minute and ID; each records this attempt and "no_available_qualified_staff", and takes the
        first idle, available, on-shift member (in ID order) with a skill of its care kind, or for
        an urgent one the first such member on a routed background task, whose task is interrupted
        (a `staff_operation_preempted` record). Without one the minute's exposure counts it (a
        qualified member busy, or one off shift or away). With a route resolver, no route is
        "route_unavailable_at_assignment"; a task that would end at or past the Run's end is
        "insufficient_run_time"; a trip the travel scheduler rejects (or can't make) is
        "resident_travel_schedule_rejected". An assigned request gets its `staff-support-{sequence}`
        task, the member's queue and busy minute, and a `staff_support_complete` event. The
        minute's metrics (no qualified worker, busy and off-shift exposure, fairness promotions)
        are counted once per minute. With `validate_state`, the world is validated at the end.
        """


def complete_support_task(
    world: WorldState,
    *,
    request_id: str,
    agent_id: str,
    staff_id: str,
    task_id: str,
    minute: int,
    cooldown_minutes: int,
    require_colocated: bool = False,
) -> None:
    """Release one staff member and resolve its matching Agent request."""
    request = world.support_requests.get(request_id)
    agent = world.agents.get(agent_id)
    person = world.staff.get(staff_id)
    task = world.active_tasks.get(task_id)
    if request is None or request.status is not SupportRequestStatus.ASSIGNED:
        raise ValueError(f"support request {request_id!r} is not assigned")
    if (
        agent is None
        or person is None
        or task is None
        or task.kind != "staff_support"
        or request.agent_id != agent_id
        or request.assigned_staff_id != staff_id
        or request.task_id != task_id
        or task.agent_id != agent_id
        or task.staff_id != staff_id
        or task.due_minute != minute
        or person.active_task_id != task_id
        or (require_colocated and agent.activity is not Activity.SEEKING_HELP)
        or (require_colocated and agent.zone_id != person.zone_id)
    ):
        raise ValueError("support completion does not match its assigned task")
    del world.active_tasks[task_id]
    person.work_queue.clear()
    person.active_task_id = None
    person.busy_until = 0
    request.status = SupportRequestStatus.RESOLVED
    request.resolved_minute = minute
    request.cooldown_until = minute + cooldown_minutes
    agent.activity = Activity.IDLE
    agent.target_id = None
    agent.support_request_id = None
    agent.support_cooldown_until = request.cooldown_until
    world.metric_state["support_resolved_count"] = (
        world.metric_state.get("support_resolved_count", 0.0) + 1.0
    )


def _support_kind(world: WorldState, request_id: str) -> str:
    care_receipt = care_receipt_for_request(world, request_id)
    return (
        care_receipt.get("support_kind", "emotional_support")
        if care_receipt is not None
        else "emotional_support"
    )


def support_required_skills(world: WorldState, request_id: str) -> set[str]:
    """Return the declared qualification alternatives for one support request."""
    return _CARE_SKILL_ALTERNATIVES.get(_support_kind(world, request_id), {"support"})


def support_wait_reason_codes(
    world: WorldState,
    request_id: str,
    *,
    minute: int | None = None,
    staff_memo: dict[str, Any] | None = None,
) -> tuple[str, ...]:
    """Describe current observable blockers without asserting a single cause.

    ``staff_memo`` keeps what is read about the staff for the next request of the same kind,
    for a caller that does not change the world between the calls.
    """
    request = world.support_requests.get(request_id)
    if request is None:
        return ("request_missing",)
    agent = world.agents.get(request.agent_id)
    sample_minute = world.minute_index if minute is None else minute
    if request.status is SupportRequestStatus.ASSIGNED:
        return (
            ("resident_travel",)
            if agent is not None and agent.activity is Activity.MOVING
            else ("support_in_progress",)
        )
    if request.status is not SupportRequestStatus.PENDING:
        return ("request_closed",)
    support_kind = _support_kind(world, request_id)
    known = staff_memo.get(support_kind) if staff_memo is not None else None
    if known is None:
        required = _CARE_SKILL_ALTERNATIVES.get(support_kind, {"support"})
        qualified = [staff for staff in world.staff.values() if required.intersection(staff.skills)]
        staff_blockers: set[str] = set()
        # The staff records alone, of a detached namespace too (state_mirror.read_only_part).
        operation_staff = read_only_part(world, "staff_operations_v1", "staff", {})
        # Read only: a detached namespace gives a document of its row and builds no tree.
        dynamics_records = read_only_part(world, "staff_dynamics_v1", "staff", {})
        for staff in qualified:
            if not staff_is_available(world, staff.id):
                staff_blockers.add("staff_unavailable")
            if staff.active_task_id is not None:
                staff_blockers.add("staff_busy")
            operation = operation_staff.get(staff.id)
            if isinstance(operation, dict) and operation.get("phase") != "idle":
                staff_blockers.add("staff_on_other_operation")
            dynamics_staff = dynamics_records.get(staff.id)
            if (
                isinstance(dynamics_staff, dict)
                and dynamics_staff.get("rest_until_minute", 0) > sample_minute
            ):
                staff_blockers.add("staff_resting")
            elif not staff_dynamics_on_shift(world, staff.id, sample_minute):
                staff_blockers.add("staff_off_shift")
        known = (qualified, staff_blockers, _route_candidates(world, qualified, sample_minute))
        if staff_memo is not None:
            staff_memo[support_kind] = known
    qualified, staff_blockers, candidates = known
    if not qualified:
        return ("no_qualified_staff",)
    route_reasons, route_is_feasible = _support_route_wait_reasons(
        world, agent_id=request.agent_id, candidates=candidates, minute=sample_minute
    )
    blockers = staff_blockers | route_reasons
    if not blockers:
        if not route_is_feasible:
            return ("no_route_to_qualified_staff",)
        if request.last_assignment_attempt_minute == sample_minute:
            if request.last_assignment_wait_reason_code:
                return (request.last_assignment_wait_reason_code,)
            return ("assignment_failure_reason_not_recorded",)
        if world.metric_state.get("support_assignment_metrics_minute", -1.0) != sample_minute:
            return ("assignment_not_attempted_at_sampled_minute",)
        return ("assignment_failure_reason_not_recorded",)
    return tuple(sorted(blockers))


def _route_candidates(world: WorldState, staff: list[Any], minute: int) -> list[Any]:
    return [
        person
        for person in staff
        if person.active_task_id is None
        and staff_is_available(world, person.id)
        and staff_dynamics_is_available(world, person.id, minute)
        and staff_dynamics_on_shift(world, person.id, minute)
    ]


def _support_route_wait_reasons(
    world: WorldState,
    *,
    agent_id: str,
    candidates: list[Any],
    minute: int,
) -> tuple[set[str], bool]:
    """Classify routed-support blockers using the same graph constraints as the runner."""
    agent = world.agents.get(agent_id)
    if agent is None:
        return set(), False
    if not candidates:
        return set(), False

    from sheltersim.domain.movement import route_is_physically_clear, route_with_movement_time
    from sheltersim.domain.zones import ZoneGraph
    from sheltersim.policies.space import SpaceRouter

    capacities = world.model_configs.get("layout_routing", {}).get(
        "max_agents_in_transit_by_edge", {}
    )
    graph = ZoneGraph.from_states(world.zones, world.zone_edges, edge_capacities=capacities)
    if "facility_geometry" in world.model_configs:
        inaccessible = {
            edge.id
            for edge in graph.edges
            if not route_is_physically_clear(world, edge.id, mobility=agent.profile.mobility.value)
        }
        graph = ZoneGraph(
            zones=graph.zones,
            edges=tuple(edge for edge in graph.edges if edge.id not in inaccessible),
        )
    router = SpaceRouter(graph)
    edge_loads: dict[str, int] = {}
    for event in world.event_queue.pending():
        if event.event_type != "agent_travel_complete" or event.due_minute <= minute:
            continue
        for edge_id in event.payload.get("route_edge_ids", ()):
            if isinstance(edge_id, str):
                edge_loads[edge_id] = edge_loads.get(edge_id, 0) + 1
    for record in read_only_part(world, "staff_operations_v1", "staff", {}).values():
        if not isinstance(record, dict) or record.get("phase") != "travel":
            continue
        for edge_id in record.get("route_edge_ids", ()):
            if isinstance(edge_id, str):
                edge_loads[edge_id] = edge_loads.get(edge_id, 0) + 1

    config = world.model_configs.get("staff", {})
    service_minutes = config.get("support_task_minutes", 10)
    reasons: set[str] = set()
    route_is_feasible = False
    for destination in sorted({person.zone_id for person in candidates}):
        route = router.route(agent.zone_id, destination, agent.id, edge_loads=edge_loads)
        if route is None:
            unconstrained = router.route(agent.zone_id, destination, agent.id, edge_loads={})
            reasons.add(
                "route_capacity_full"
                if unconstrained is not None
                else "no_route_to_qualified_staff"
            )
            continue
        route = route_with_movement_time(world, agent.id, route)
        zone = world.zones[destination]
        if destination != agent.zone_id and zone.capacity is not None:
            occupied = sum(person.zone_id == destination for person in world.agents.values())
            incoming = sum(
                person.activity is Activity.MOVING and person.target_id == destination
                for person in world.agents.values()
            )
            if occupied + incoming >= zone.capacity:
                reasons.add("staff_destination_full")
                continue
        if minute + route.travel_minutes + service_minutes >= world.duration_minutes:
            reasons.add("insufficient_run_time")
            continue
        route_is_feasible = True
    return reasons, route_is_feasible


def support_waited_minutes(world: WorldState, request_id: str, *, minute: int | None = None) -> int:
    """Return elapsed wait, stopping at service start once a resident arrives."""
    request = world.support_requests.get(request_id)
    if request is None:
        return 0
    sample_minute = world.minute_index if minute is None else minute
    endpoint = sample_minute
    if request.status is SupportRequestStatus.ASSIGNED:
        agent = world.agents.get(request.agent_id)
        task = world.active_tasks.get(request.task_id or "")
        if agent is not None and agent.activity is not Activity.MOVING and task is not None:
            endpoint = min(sample_minute, task.start_minute)
    return max(0, endpoint - request.requested_minute)


def default_support_fairness_minutes(unresolved_after_minutes: int) -> int:
    """Pick a bounded default aging threshold that precedes shorter timeouts."""
    if type(unresolved_after_minutes) is not int or unresolved_after_minutes <= 0:
        return 30
    return max(1, min(30, unresolved_after_minutes - 1))

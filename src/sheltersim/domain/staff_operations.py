"""Autonomous, routed staff assignments with bounded causal task histories.

The rules of the tick are native/src/rules/staff.rs's and staff_operation_events.rs's kernels
(`kernel_only`): a value they refuse is KernelRefusal, before anything changes. The checks of a
config, of a saved namespace and of an operator's order stay here.
"""

from __future__ import annotations

import math
from typing import Any

from sheltersim import native
from sheltersim.contracts.operations import SHARED_OPERATIONS_KEY
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import WorldState
from sheltersim.domain.models import ActiveTask
from sheltersim.domain.movement import shared_passage_edge_loads
from sheltersim.domain.operational import staff_is_available
from sheltersim.domain.staff_dynamics import staff_dynamics_is_available
from sheltersim.events.models import EventKind, ScheduledEvent
from sheltersim.policies.space import SpaceRouter

STAFF_OPERATIONS_STATE_KEY = "staff_operations_v1"
STAFF_OPERATIONS_POLICY_VERSION = "autonomous-routed-staff-v1"
STAFF_OPERATIONS_SOURCE_ID = "ASM-STAFF-OPERATIONS-V1"
MAX_STAFF_HISTORY = 20_000
STAFF_RELIEF_POLICY_VERSION = "same-site-staff-relief-v1"
STAFF_RELIEF_SOURCE_ID = "ASM-STAFF-RELIEF-V1"

_DURATION_DEFAULTS = {
    "cleaning": 15,
    "maintenance": 20,
    "information_outreach": 10,
    "intake": 5,
    "patrol": 15,
}
_TASK_SKILLS = {
    "cleaning": {"cleaning", "sanitation"},
    "maintenance": {"maintenance", "repair"},
    "information_outreach": {"support", "information"},
    "intake": {"support", "intake"},
    "patrol": {"support", "patrol"},
}
_FIELDS = {
    "schema_version",
    "policy_version",
    "source_id",
    "config",
    "zones",
    "staff",
    "history",
}


def canonical_staff_operations_config(raw: Any) -> dict[str, Any]:
    if raw is None:
        raw = {}
    if not isinstance(raw, dict) or set(raw) - {
        "schema_version",
        "policy_version",
        "source_id",
        "task_duration_minutes",
        "sanitation_use_per_person_minute",
        "cleaning_sanitation_recovery",
        "outreach_uncertainty_relief",
        "patrol_reassurance",
        "thresholds_by_zone",
        "effects_by_zone",
    }:
        raise ValueError("staff_operations config has unsupported fields")
    config = {
        "schema_version": 1,
        "policy_version": STAFF_OPERATIONS_POLICY_VERSION,
        "source_id": STAFF_OPERATIONS_SOURCE_ID,
        "task_duration_minutes": dict(_DURATION_DEFAULTS),
        "sanitation_use_per_person_minute": 0.000005,
        "cleaning_sanitation_recovery": 0.15,
        "outreach_uncertainty_relief": 0.04,
        "patrol_reassurance": 0.01,
        "thresholds_by_zone": {},
        "effects_by_zone": {},
        **raw,
    }
    if (
        config["schema_version"] != 1
        or config["policy_version"] != STAFF_OPERATIONS_POLICY_VERSION
        or config["source_id"] != STAFF_OPERATIONS_SOURCE_ID
    ):
        raise ValueError("unsupported staff_operations policy or source")
    durations = config["task_duration_minutes"]
    if not isinstance(durations, dict) or set(durations) != set(_DURATION_DEFAULTS):
        raise ValueError("staff_operations task durations must define every registered task")
    for kind, value in durations.items():
        if type(value) is not int or not 1 <= value <= 24 * 60:
            raise ValueError(f"staff_operations duration for {kind} is outside 1..1440 minutes")
    for name in (
        "sanitation_use_per_person_minute",
        "cleaning_sanitation_recovery",
        "outreach_uncertainty_relief",
        "patrol_reassurance",
    ):
        value = config[name]
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(value)
            or not 0.0 <= value <= 1.0
        ):
            raise ValueError(f"staff_operations {name} must be finite and within 0..1")
    thresholds_by_zone = config["thresholds_by_zone"]
    if not isinstance(thresholds_by_zone, dict):
        raise ValueError("staff_operations thresholds_by_zone must be an object")
    allowed_thresholds = {
        "cleaning_sanitation_below",
        "outreach_uncertainty_at_least",
        "patrol_stress_at_least",
    }
    for zone_id, thresholds in thresholds_by_zone.items():
        if (
            not isinstance(zone_id, str)
            or not zone_id
            or not isinstance(thresholds, dict)
            or not thresholds
            or set(thresholds) - allowed_thresholds
        ):
            raise ValueError("staff_operations per-zone threshold entry is invalid")
        for name, value in thresholds.items():
            if (
                not isinstance(value, (int, float))
                or isinstance(value, bool)
                or not math.isfinite(value)
                or not 0.0 <= value <= 1.0
            ):
                raise ValueError(f"staff_operations per-zone {name} must be finite and within 0..1")
    effects_by_zone = config["effects_by_zone"]
    allowed_effects = {
        "sanitation_use_per_person_minute": (0.0, 0.01),
        "cleaning_sanitation_recovery": (0.0, 1.0),
        "outreach_uncertainty_relief": (0.0, 1.0),
        "patrol_reassurance": (0.0, 1.0),
    }
    if not isinstance(effects_by_zone, dict):
        raise ValueError("staff_operations effects_by_zone must be an object")
    for zone_id, effects in effects_by_zone.items():
        if (
            not isinstance(zone_id, str)
            or not zone_id
            or not isinstance(effects, dict)
            or not effects
            or set(effects) - set(allowed_effects)
        ):
            raise ValueError("staff_operations per-zone effect entry is invalid")
        for name, value in effects.items():
            lower, upper = allowed_effects[name]
            if (
                not isinstance(value, (int, float))
                or isinstance(value, bool)
                or not math.isfinite(value)
                or not lower <= value <= upper
            ):
                raise ValueError(
                    f"staff_operations per-zone {name} must be finite and within "
                    f"{lower:g}..{upper:g}"
                )
    return config


def create_staff_operations_state(world: WorldState) -> dict[str, Any]:
    """Initialize team workload and a sanitation index without changing staff records."""
    config = canonical_staff_operations_config(world.model_configs.get("staff_operations"))
    return {
        "schema_version": 1,
        "policy_version": STAFF_OPERATIONS_POLICY_VERSION,
        "source_id": STAFF_OPERATIONS_SOURCE_ID,
        "config": config,
        "zones": {
            zone_id: {"sanitation": 1.0, "updated_minute": world.minute_index}
            for zone_id in sorted(world.zones)
        },
        "staff": {staff_id: _idle_record() for staff_id in sorted(world.staff)},
        "history": [],
    }


def validate_staff_operations_state(world: WorldState) -> None:
    state = world.model_state.get(STAFF_OPERATIONS_STATE_KEY)
    if state is None:
        return
    if (
        not isinstance(state, dict)
        or set(state) != _FIELDS
        or state.get("schema_version") != 1
        or state.get("policy_version") != STAFF_OPERATIONS_POLICY_VERSION
        or state.get("source_id") != STAFF_OPERATIONS_SOURCE_ID
        or not isinstance(state.get("zones"), dict)
        or set(state["zones"]) != set(world.zones)
        or not isinstance(state.get("staff"), dict)
        or set(state["staff"]) != set(world.staff)
        or not isinstance(state.get("history"), list)
        or len(state["history"]) > MAX_STAFF_HISTORY
    ):
        raise ValueError("staff_operations_v1 has an invalid exact schema")
    config = canonical_staff_operations_config(state["config"])
    if config != state["config"]:
        legacy_config = dict(config)
        for additive_field in ("thresholds_by_zone", "effects_by_zone"):
            if additive_field not in state["config"] and not config[additive_field]:
                legacy_config.pop(additive_field, None)
        if legacy_config != state["config"]:
            raise ValueError("staff_operations_v1 config is not canonical")
    if set(config["thresholds_by_zone"]) - set(world.zones):
        raise ValueError("staff_operations_v1 per-zone threshold references an unknown zone")
    if set(config["effects_by_zone"]) - set(world.zones):
        raise ValueError("staff_operations_v1 per-zone effect references an unknown zone")
    for zone_id, zone in state["zones"].items():
        if (
            not isinstance(zone, dict)
            or set(zone) != {"sanitation", "updated_minute"}
            or not isinstance(zone["sanitation"], (int, float))
            or isinstance(zone["sanitation"], bool)
            or not math.isfinite(zone["sanitation"])
            or not 0.0 <= zone["sanitation"] <= 1.0
            or type(zone["updated_minute"]) is not int
            or zone["updated_minute"] < 0
        ):
            raise ValueError(f"staff_operations_v1 zone state for {zone_id!r} is invalid")
    fields = {
        "phase",
        "task_id",
        "task_kind",
        "target_id",
        "target_zone_id",
        "reason",
        "started_minute",
        "travel_due_minute",
        "arrival_minute",
        "due_minute",
        "route_zone_ids",
        "route_edge_ids",
        "travel_minutes",
        "causation_event_ids",
    }
    pending_events = world.event_queue.pending()
    for staff_id, record in state["staff"].items():
        if (
            not isinstance(record, dict)
            or set(record) != fields
            or record["phase"] not in {"idle", "travel", "working"}
        ):
            raise ValueError(f"staff_operations_v1 record for {staff_id!r} is invalid")
        if record["phase"] == "idle":
            if record != _idle_record():
                raise ValueError(f"idle staff_operations_v1 record for {staff_id!r} retains a task")
            continue
        if (
            record["task_id"] not in world.active_tasks
            or world.active_tasks[record["task_id"]].staff_id != staff_id
            or world.staff[staff_id].active_task_id != record["task_id"]
            or world.staff[staff_id].work_queue != [record["task_id"]]
            or world.staff[staff_id].busy_until != record["due_minute"]
            or record["task_kind"] not in _DURATION_DEFAULTS
            or record["target_zone_id"] not in world.zones
            or type(record["started_minute"]) is not int
            or type(record["due_minute"]) is not int
            or record["due_minute"] <= record["started_minute"]
            or not isinstance(record["route_zone_ids"], list)
            or not isinstance(record["route_edge_ids"], list)
            or any(zone_id not in world.zones for zone_id in record["route_zone_ids"])
            or any(edge_id not in world.zone_edges for edge_id in record["route_edge_ids"])
            or type(record["travel_minutes"]) is not int
            or record["travel_minutes"] < 0
            or not isinstance(record["causation_event_ids"], list)
            or type(record["started_minute"]) is not int
            or record["started_minute"] < 0
            or not isinstance(record["reason"], str)
            or not record["reason"]
            or (record["target_id"] is not None and record["target_id"] not in world.facilities)
        ):
            raise ValueError(f"active staff_operations_v1 record for {staff_id!r} is invalid")
        route_is_valid = (
            len(record["route_zone_ids"]) == len(record["route_edge_ids"]) + 1
            and bool(record["route_edge_ids"])
            and all(
                world.zone_edges[edge_id].from_zone_id == record["route_zone_ids"][index]
                and world.zone_edges[edge_id].to_zone_id == record["route_zone_ids"][index + 1]
                for index, edge_id in enumerate(record["route_edge_ids"])
            )
            and record["route_zone_ids"][0] == world.staff[staff_id].zone_id
            and record["route_zone_ids"][-1] == record["target_zone_id"]
            and record["travel_minutes"]
            == sum(world.zone_edges[edge_id].travel_minutes for edge_id in record["route_edge_ids"])
        )
        if record["phase"] == "travel":
            if (
                not route_is_valid
                or type(record["travel_due_minute"]) is not int
                or record["travel_due_minute"]
                != record["started_minute"] + record["travel_minutes"]
                or record["arrival_minute"] is not None
            ):
                raise ValueError(f"staff_operations_v1 travel record for {staff_id!r} is invalid")
        elif (
            record["route_zone_ids"]
            or record["route_edge_ids"]
            or record["travel_due_minute"] is not None
            or type(record["arrival_minute"]) is not int
            or record["arrival_minute"] != record["started_minute"] + record["travel_minutes"]
            or world.staff[staff_id].zone_id != record["target_zone_id"]
        ):
            raise ValueError(f"staff_operations_v1 working record for {staff_id!r} is invalid")
        expected_due = (
            record["started_minute"]
            + record["travel_minutes"]
            + config["task_duration_minutes"][record["task_kind"]]
        )
        if record["due_minute"] != expected_due:
            raise ValueError(f"staff_operations_v1 due minute for {staff_id!r} is invalid")
        completions = [
            event
            for event in pending_events
            if event.event_type == "staff_operation_complete"
            and event.payload.get("task_id") == record["task_id"]
            and event.payload.get("staff_id") == staff_id
            and event.due_minute == record["due_minute"]
        ]
        arrivals = [
            event
            for event in pending_events
            if event.event_type == "staff_operation_arrive"
            and event.payload.get("task_id") == record["task_id"]
            and event.payload.get("staff_id") == staff_id
        ]
        if (
            len(completions) != 1
            or completions[0].payload.get("task_kind") != record["task_kind"]
            or completions[0].payload.get("target_id") != record["target_id"]
            or len(arrivals) != int(record["phase"] == "travel")
            or (arrivals and arrivals[0].due_minute != record["travel_due_minute"])
            or (arrivals and arrivals[0].payload.get("target_zone_id") != record["target_zone_id"])
        ):
            raise ValueError(
                f"staff_operations_v1 event references for {staff_id!r} are inconsistent"
            )
    for item in state["history"]:
        if (
            not isinstance(item, dict)
            or set(item)
            != {
                "task_id",
                "staff_id",
                "task_kind",
                "target_id",
                "target_zone_id",
                "reason",
                "status",
                "start_minute",
                "arrival_minute",
                "complete_minute",
                "travel_minutes",
                "causation_event_ids",
            }
            or item["staff_id"] not in world.staff
            or item["task_kind"] not in _DURATION_DEFAULTS
            or item["status"] not in {"completed", "blocked", "interrupted"}
            or item["target_zone_id"] not in world.zones
            or (item["target_id"] is not None and item["target_id"] not in world.facilities)
            or not isinstance(item["task_id"], str)
            or not isinstance(item["reason"], str)
            or any(
                type(item[name]) is not int or item[name] < 0
                for name in ("start_minute", "complete_minute", "travel_minutes")
            )
            or item["complete_minute"] < item["start_minute"]
            or (
                item["arrival_minute"] is None
                and not (item["status"] == "interrupted" and item["travel_minutes"] > 0)
            )
            or (
                item["arrival_minute"] is not None
                and (
                    type(item["arrival_minute"]) is not int
                    or not item["start_minute"] <= item["arrival_minute"] <= item["complete_minute"]
                )
            )
            or not isinstance(item["causation_event_ids"], list)
            or any(
                not isinstance(event_id, str) or not event_id
                for event_id in item["causation_event_ids"]
            )
        ):
            raise ValueError("staff_operations_v1 history item is invalid")


def add_staff_operations_agent(world: WorldState, staff_id: str) -> None:
    state = world.model_state.get(STAFF_OPERATIONS_STATE_KEY)
    if state is not None and staff_id in world.staff:
        state["staff"][staff_id] = _idle_record()


@kernel_only("staff", "update_operations")
def update_staff_operations(world: WorldState, minute: int) -> None:
    """Apply occupancy-linked sanitation demand using bounded per-person rates.

    Nothing without the namespace. For each zone in ID order: its sanitation falls by
    `load * rate * elapsed`, down to 0.0, where load is the agents in it, rate its
    `effects_by_zone` rate or the config's `sanitation_use_per_person_minute` (a float, or an int
    taken as the float of its value), and elapsed the minutes since its `updated_minute` (none
    before it); `updated_minute` becomes this minute.
    """


@kernel_only("staff", "assign_operations")
def assign_staff_operations(world: WorldState, minute: int, router: SpaceRouter) -> tuple[str, ...]:
    """Choose the highest-need available task by role, urgency and reachable travel time.

    Returns the new task IDs; () without the namespace. Staff go in order of recent (240 minutes)
    and total completed tasks, fatigue (rounded to thousandths) and ID; each one idle, available
    and on shift (`staff_dynamics_is_available`) takes, of the tasks its skills allow that no
    other task holds (cleaning a zone below its sanitation threshold, maintaining a facility with
    units out and no outage or modifier, information outreach to a zone whose mean uncertainty is
    at least its threshold, a patrol of a zone whose mean stress is), the one the staff-operations
    formula ranks first among those the router reaches with the passages' loads. A task that would
    end at or past the Run's end is not taken. It writes the operation record, the active task and
    the staff member's queue, and schedules `staff_operation_arrive` (when there is travel) and
    `staff_operation_complete`, with the formula's source and policy as provenance; with
    `movement_v1`, the staff positions after the minute.
    """


_TASK_NAMES = {
    "cleaning": "清掃",
    "maintenance": "保守",
    "information_outreach": "情報提供",
    "intake": "受付",
    "patrol": "巡回",
}


def _missing_task_skill_message(world: WorldState, staff_id: str, task_kind: str) -> str:
    """Tell the operator, before approval, who could take an order this worker cannot (#75).

    Generated Scenarios staff no one for cleaning or maintenance: in a shelter that
    work falls to evacuee rotas, the facility manager or contractors, not to staff.
    """
    skills = _TASK_SKILLS[task_kind]
    name = _TASK_NAMES[task_kind]
    skill_list = "・".join(sorted(skills))
    qualified = sorted(
        other_id for other_id, other in world.staff.items() if skills.intersection(other.skills)
    )
    if qualified:
        return (
            f"staff_lacks_task_skill: {staff_id}は{name}の技能（{skill_list}）を持ちません。"
            f"技能を持つ職員（{'、'.join(qualified)}）に指示してください。"
        )
    advice = "担当変更（set_staff_role）で職員に技能を与えてから指示してください。"
    if task_kind == "maintenance":
        advice = (
            "設備の修理は修理の手配（facility_repair）で指示するか、"
            "担当変更（set_staff_role）で職員に技能を与えてから指示してください。"
        )
    return f"no_qualified_staff: {name}の技能（{skill_list}）を持つ職員がいません。{advice}"


def validate_staff_operation_order(
    world: WorldState,
    *,
    staff_id: str,
    task_kind: str,
    target_zone_id: str,
    target_id: str | None,
    minute: int,
) -> Any:
    """Validate an operator-ordered task and return its reachable staff route."""
    from sheltersim.domain.movement import route_is_physically_clear
    from sheltersim.domain.zones import ZoneGraph
    from sheltersim.events.models import EventApplicationError
    from sheltersim.policies.space import SpaceRouter

    state = world.model_state.get(STAFF_OPERATIONS_STATE_KEY)
    staff = world.staff.get(staff_id)
    record = state.get("staff", {}).get(staff_id) if isinstance(state, dict) else None
    if state is None or SHARED_OPERATIONS_KEY not in world.model_state:
        raise EventApplicationError("staff task order requires staff and shared operation state")
    if staff is None or not isinstance(record, dict):
        raise EventApplicationError("staff task order references unknown staff")
    if (
        record.get("phase") != "idle"
        or staff.active_task_id is not None
        or not staff_is_available(world, staff_id)
        or not staff_dynamics_is_available(world, staff_id, minute)
    ):
        raise EventApplicationError("staff task order requires an idle, available, on-shift worker")
    if task_kind not in _TASK_SKILLS:
        raise EventApplicationError("staff task order uses an unregistered task kind")
    if not _TASK_SKILLS[task_kind].intersection(staff.skills):
        raise EventApplicationError(_missing_task_skill_message(world, staff_id, task_kind))
    if target_zone_id not in world.zones:
        raise EventApplicationError("staff task order references an unknown target zone")
    if task_kind == "maintenance":
        facility = world.facilities.get(target_id or "")
        if facility is None or facility.zone_id != target_zone_id:
            raise EventApplicationError(
                "maintenance order must target a facility in its selected zone"
            )
        if facility.working_units >= facility.capacity or _maintenance_blocked(world, facility.id):
            raise EventApplicationError(
                "maintenance order has no currently repairable facility unit"
            )
    elif target_id is not None:
        raise EventApplicationError("only maintenance orders accept a target facility")
    if task_kind in {"information_outreach", "patrol"}:
        if not any(agent.zone_id == target_zone_id for agent in world.agents.values()):
            raise EventApplicationError("staff support order requires residents in its target zone")
        if task_kind == "information_outreach" and not world.model_state.get(
            "body_state_v1", {}
        ).get("agents"):
            raise EventApplicationError("information outreach requires the body-state policy")
    if task_kind == "intake":
        population = world.model_state.get("population_dynamics_v1")
        if not isinstance(population, dict) or not population.get("pending_arrivals"):
            raise EventApplicationError("intake order requires pending arrivals")
    graph = ZoneGraph.from_states(world.zones, world.zone_edges)
    if "facility_geometry" in world.model_configs:
        graph = ZoneGraph(
            zones=graph.zones,
            edges=tuple(
                edge
                for edge in graph.edges
                if route_is_physically_clear(world, edge.id, mobility="independent")
            ),
        )
    router = SpaceRouter(graph)
    route = router.route(
        staff.zone_id,
        target_zone_id,
        staff_id,
        edge_loads=shared_passage_edge_loads(world, minute),
    )
    if route is None:
        raise EventApplicationError("staff task order has no reachable route")
    duration = state["config"]["task_duration_minutes"][task_kind]
    if minute + route.travel_minutes + duration >= world.duration_minutes:
        raise EventApplicationError("staff task order would finish beyond the Run boundary")
    return route


def order_staff_operation(
    world: WorldState,
    *,
    staff_id: str,
    task_kind: str,
    target_zone_id: str,
    target_id: str | None,
    reason: str,
    causation_event_id: str,
    source_id: str | None,
) -> str:
    """Start one explicit typed staff task using the normal routed task lifecycle."""
    from sheltersim.events.models import EventApplicationError, ScheduledEvent
    from sheltersim.policies.registry import create_staff_operations_formula

    route = validate_staff_operation_order(
        world,
        staff_id=staff_id,
        task_kind=task_kind,
        target_zone_id=target_zone_id,
        target_id=target_id,
        minute=world.minute_index,
    )
    state = world.model_state[STAFF_OPERATIONS_STATE_KEY]
    duration = state["config"]["task_duration_minutes"][task_kind]
    due_minute = world.minute_index + route.travel_minutes + duration
    if due_minute >= world.duration_minutes:
        raise EventApplicationError("staff task order would finish beyond the Run boundary")
    formula = create_staff_operations_formula(world.model_configs)
    selected = "staff_operations" in world.model_configs.get("policy_registry", {})
    formula_source_id = formula.source_id if selected else STAFF_OPERATIONS_SOURCE_ID
    formula_policy_id = formula.model_id if selected else STAFF_OPERATIONS_POLICY_VERSION
    operation_source_id = source_id or formula_source_id
    sequence = world.allocate_event_sequence()
    task_id = f"staff-operation-{sequence}"
    arrival_minute = world.minute_index + route.travel_minutes
    state["staff"][staff_id] = {
        "phase": "travel" if route.travel_minutes else "working",
        "task_id": task_id,
        "task_kind": task_kind,
        "target_id": target_id,
        "target_zone_id": target_zone_id,
        "reason": reason,
        "started_minute": world.minute_index,
        "travel_due_minute": arrival_minute if route.travel_minutes else None,
        "arrival_minute": None if route.travel_minutes else arrival_minute,
        "due_minute": due_minute,
        "route_zone_ids": list(route.zone_ids) if route.travel_minutes else [],
        "route_edge_ids": list(route.edge_ids) if route.travel_minutes else [],
        "travel_minutes": route.travel_minutes,
        "causation_event_ids": [causation_event_id],
    }
    world.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="staff_operation",
        agent_id=None,
        resource_id=None,
        start_minute=world.minute_index,
        due_minute=due_minute,
        staff_id=staff_id,
    )
    staff = world.staff[staff_id]
    staff.active_task_id = task_id
    staff.work_queue[:] = [task_id]
    staff.busy_until = due_minute
    provenance = {
        "source_type": "assumption",
        "source_id": formula_source_id,
        "policy_version": formula_policy_id,
        "reason": reason,
    }
    if route.travel_minutes:
        world.schedule_event(
            ScheduledEvent(
                event_id=f"staff-operation-arrive-{sequence}",
                kind=EventKind.COMPLETION,
                due_minute=arrival_minute,
                priority=5,
                sequence=sequence,
                event_type="staff_operation_arrive",
                payload={
                    "staff_id": staff_id,
                    "task_id": task_id,
                    "target_zone_id": target_zone_id,
                },
                source_id=operation_source_id,
                causation_id=causation_event_id,
                provenance=provenance,
            )
        )
    completion_sequence = world.allocate_event_sequence()
    world.schedule_event(
        ScheduledEvent(
            event_id=f"staff-operation-complete-{completion_sequence}",
            kind=EventKind.COMPLETION,
            due_minute=due_minute,
            priority=5,
            sequence=completion_sequence,
            event_type="staff_operation_complete",
            payload={
                "staff_id": staff_id,
                "task_id": task_id,
                "task_kind": task_kind,
                "target_id": target_id,
                "target_zone_id": target_zone_id,
            },
            source_id=operation_source_id,
            causation_id=causation_event_id,
            provenance=provenance,
        )
    )
    if "movement_v1" in world.model_state:
        from sheltersim.domain.movement import staff_position_snapshots

        staff_position_snapshots(world, boundary_minute=world.minute_index + 1)
    return task_id


@kernel_only("staff", "relief")
def schedule_staff_operation_relief(world: WorldState, minute: int) -> tuple[str, ...]:
    """Schedule same-site relief before a working or traveling staff member goes off shift.

    A replacement already at the task destination can take over the same typed task.
    If the former worker was still traveling, the replacement starts the work interval
    from that destination; no partial task effect is inferred or retained.

    Returns the IDs of the `staff_operation_relief` events it scheduled for the next minute; () when
    there is no namespace or the next minute is the Run's end. Only for a task with no relief
    pending whose worker is unavailable or off shift at the next minute, that is theirs, ends after
    it and whose work fits before the Run's end; the replacement is the idle, available member at
    its target zone with a skill of the task and the least recent and total work, fatigue and ID.
    """


@kernel_only("staff_operation_events", "interrupt_trips")
def interrupt_staff_trips_on_closed_routes(
    world: WorldState, event: ScheduledEvent
) -> tuple[str, ...]:
    """Stop travel tasks invalidated by a newly applied closure for same-tick reassignment.

    The caller invokes this only after the typed geometry event was applied. Existing
    interruption logic records the incomplete work and moves staff to the nearest
    reached zone. The ordinary allocator then considers the work again using the
    refreshed, closure-aware graph.

    Returns the staff IDs whose travel it interrupted (`interrupt_staff_operation`), in ID order:
    those with an edge of their route gone, closed or not physically clear for independent
    walkers. With `movement_v1`, the staff positions at the event's minute.
    """


@kernel_only("staff_operation_events", "interrupt")
def interrupt_staff_operation(
    world: WorldState,
    staff_id: str,
    event: ScheduledEvent,
    *,
    current_event_id: str | None = None,
) -> bool:
    """Cancel an autonomous task and its pending events at an unavailability boundary.

    False for an unknown or idle staff member. A traveling one moves to the route's zone nearest
    where the event's minute finds them (past half an edge is its end). Its pending arrive and
    complete events, except `current_event_id`, leave the queue as SUPPRESSED records ("Staff
    operation was interrupted by staff unavailability"). The history gets an "interrupted" item
    (causes: the record's, the event and the removed events, sorted), kept to MAX_STAFF_HISTORY;
    the task, the queue and the busy minute are cleared and the record is idle again. With
    `movement_v1`, the staff positions at the event's minute.
    """


def _maintenance_blocked(world: WorldState, facility_id: str) -> bool:
    from sheltersim.domain.infrastructure import active_facility_outage
    from sheltersim.events.effects import has_active_facility_modifier

    return active_facility_outage(world, facility_id) or has_active_facility_modifier(
        world, facility_id
    )


def _idle_record() -> dict[str, Any]:
    return {
        "phase": "idle",
        "task_id": None,
        "task_kind": None,
        "target_id": None,
        "target_zone_id": None,
        "reason": None,
        "started_minute": None,
        "travel_due_minute": None,
        "arrival_minute": None,
        "due_minute": None,
        "route_zone_ids": [],
        "route_edge_ids": [],
        "travel_minutes": 0,
        "causation_event_ids": [],
    }


register_rust_validator(STAFF_OPERATIONS_STATE_KEY, native.state("staff_operations").validate)

"""The staff's assignment in Rust: the Python entries, and a preemption in a Run.

The rules' cases (a task, a relief, a route's wait, the support wait's reasons) are tested in the
core without Python (native/core/tests/staff_assignment.rs, Issue #225), on the worlds this file
built (`tests/support/core_fixtures_service.py`, whose builders this file uses). This file keeps
one test of each Python entry that reaches the kernel and gives its answer
(`assign_staff_operations`, `schedule_staff_operation_relief`, `_support_wait_snapshot`,
`StaffService.assign_pending` with the controller's own route and trip), and the boundary:
`format_g` against CPython's `format`, the refusals, the service's own check of the minute, and
Runs through the driver. `assign_staff_operations`, `schedule_staff_operation_relief` and the
controller's `_route` have no Python body left (lane L10): what they start is checked against a
digest of what the Python rule started on the same world (its answer, the world, its records and
events), taken before it went, pinned on every OS as the gate's hashes are. No Run reaches the
preemption of a background task, so a Run here does, through the driver.
"""

from __future__ import annotations

import hashlib
import json
import random
from pathlib import Path
from typing import Any

import pytest
from support.core_fixtures_service import (
    capacity_controller,
    member,
    on_a_task,
    pending_request,
    router_of,
    routing,
    spread,
    staff_document,
    staff_world,
    trip_event,
)
from support.world import driven_ticks

from sheltersim import native
from sheltersim.core.engine import TickEngine, TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.models import SupportRequestStatus
from sheltersim.domain.staff import StaffService
from sheltersim.domain.staff_operations import (
    STAFF_OPERATIONS_STATE_KEY,
    assign_staff_operations,
    schedule_staff_operation_relief,
)
from sheltersim.experiments import metrics
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.inspection.traces import RunArtifactWriter

KEY = STAFF_OPERATIONS_STATE_KEY


def _state(world: WorldState) -> dict[str, Any]:
    return {
        "world": json.loads(world._canonical_text(world._canonical_data())),
        "records": [raw.decode() for raw in world.event_log.items_bytes(0, len(world.event_log))],
        "pending": [event.to_dict() for event in world.event_queue.pending()],
        "counters": dict(world.id_counters),
    }


# What the Python rules started, by the label of the call: a digest of the answer and `_state`
# after it, taken before they went (lane L10).
PYTHON_DIGESTS = {
    "zone_threshold": "3e1aecc8255f15d4",
    "relief": "708bac4aa204b371",
}


def _is_what_python_did(label: str, answer: Any, world: WorldState) -> None:
    text = json.dumps([list(answer), _state(world)])
    assert hashlib.sha256(text.encode()).hexdigest()[:16] == PYTHON_DIGESTS[label]


def _assign(label: str, world: WorldState, minute: int, router: Any = None) -> Any:
    answer = assign_staff_operations(world, minute, router or router_of(world))
    _is_what_python_did(label, answer, world)
    assert world._core.mirror(KEY).verify()
    return answer


def _relief(label: str, world: WorldState, minute: int) -> Any:
    answer = schedule_staff_operation_relief(world, minute)
    _is_what_python_did(label, answer, world)
    return answer


# ---- assign_staff_operations ----


def test_a_format_g_is_python_s() -> None:
    kernel = native.rules("staff").format_g
    generator = random.Random(20261102)
    values = [0.0, -0.0, 1.0, 0.85, 0.45, 0.6, 1e-5, 0.0001, 0.00001234, 123456.0, 1234567.0]
    values += [1e6, 999999.5, 1e-300, 0.1 + 0.2, 1.5, 2.5, 0.000012345678, 100000.0, 12.5]
    values += [generator.uniform(0, 1) for _ in range(2000)]
    values += [10 ** generator.uniform(-8, 9) for _ in range(2000)]
    values += [round(generator.uniform(0, 1), generator.randrange(1, 7)) for _ in range(500)]
    for value in values:
        assert kernel(value) == format(value, "g")


def test_a_zone_s_threshold_names_the_reason_as_python_formats_it() -> None:
    thresholds = {"toilet": {"cleaning_sanitation_below": 1}}

    def make() -> WorldState:
        world = staff_world(
            staff=[member("S01", "toilet", "cleaning")],
            configs={"staff_operations": {"thresholds_by_zone": thresholds}},
        )
        world.model_state[KEY]["zones"]["toilet"]["sanitation"] = 0.99
        return world

    first = make()
    assert _assign("zone_threshold", first, 0)
    assert first.model_state[KEY]["staff"]["S01"]["reason"] == "sanitation_index_below_1"


def test_an_unknown_formula_and_a_corrupt_value_are_refused() -> None:
    """A fatigue no rule writes (the Python rule's `round` raised OverflowError on it) and a formula
    the kernel doesn't know: KernelRefusal, with nothing changed."""
    world = staff_world(staff=[member("S01", "toilet", "patrol")])
    spread(world, "toilet", 0.9)
    before = json.dumps(_state(world))
    world.model_state["staff_dynamics_v1"]["staff"]["S01"]["fatigue"] = float("inf")
    with pytest.raises(native.Unsupported):
        native.rules("staff").assign_operations(world, 0, router_of(world))
    with pytest.raises(KernelRefusal):
        assign_staff_operations(world, 0, router_of(world))
    world.model_state["staff_dynamics_v1"]["staff"]["S01"]["fatigue"] = 0.0
    assert json.dumps(_state(world)) == before
    world.model_configs["policy_registry"] = {"staff_operations": "a-later-formula"}
    with pytest.raises(KernelRefusal):
        assign_staff_operations(world, 0, router_of(world))


# ---- schedule_staff_operation_relief ----


def test_a_relief_goes_to_the_replacement_the_python_rule_picks() -> None:
    first = on_a_task()
    first.minute_index = 1
    answer = _relief("relief", first, 1)
    assert len(answer) == 1
    # The one scheduled is not asked for again, and neither is one at a later minute.
    assert schedule_staff_operation_relief(first, 1) == ()
    assert next(
        event.payload["replacement_staff_id"]
        for event in first.event_queue.pending()
        if event.event_type == "staff_operation_relief"
    ) in {"S02", "S03"}


# ---- What the metrics read, with routes ----
#
# `_support_wait_snapshot` has no Python rule left (lane L10, `kernel_only`). The reasons below
# are the Python rule's.


def _both_snapshots(world: WorldState, minute: int) -> dict[str, Any]:
    active = sorted(
        (
            request
            for request in world.support_requests.values()
            if request.status in (SupportRequestStatus.PENDING, SupportRequestStatus.ASSIGNED)
        ),
        key=lambda request: request.id,
    )
    return metrics._support_wait_snapshot(world, active, minute)[0]


def _reasons(world: WorldState, minute: int = 0) -> list[list[str]]:
    return [detail["reason_codes"] for detail in _both_snapshots(world, minute).values()]


def test_the_reasons_a_free_worker_s_route_gives_are_the_python_rule_s() -> None:
    world = staff_world(staff=[member("S01", "toilet", "support")], duration=200)
    people = sorted(world.agents)
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    pending_request(world, 1, people[0])
    assert _reasons(world) == [["assignment_not_attempted_at_sampled_minute"]]
    world.metric_state["support_assignment_metrics_minute"] = 0.0
    assert _reasons(world) == [["assignment_failure_reason_not_recorded"]]
    request = world.support_requests["support-1"]
    request.last_assignment_attempt_minute = 0
    assert _reasons(world) == [["assignment_failure_reason_not_recorded"]]
    request.last_assignment_wait_reason_code = "route_unavailable_at_assignment"
    assert _reasons(world) == [["route_unavailable_at_assignment"]]


# ---- assign_pending ----


def test_a_free_worker_or_a_priority_hands_the_step_to_assign_work() -> None:
    world = staff_world(staff=[member("S01", "toilet", "support")])
    request = pending_request(world, 1, sorted(world.agents)[0])
    service = StaffService(world)
    before = json.dumps(_state(world))
    kernel = native.rules("staff").assign_none
    assert kernel(service, 0, validate_state=False) is None
    assert kernel(service, 0) is None
    assert kernel(service, 1, validate_state=False) is None
    assert json.dumps(_state(world)) == before
    world.staff["S01"].active_task_id = "busy"
    world.model_state["llm"] = {"support_priorities": {request.id: 1}}
    assert kernel(service, 0, validate_state=False) is None
    world.model_state["llm"] = {"support_priorities": {}}
    assert kernel(service, 0, validate_state=False) == ()


def test_a_wrong_minute_is_the_python_rule_s_error() -> None:
    world = staff_world(staff=[member("S01", "toilet", "support")])
    service = StaffService(world)
    with pytest.raises(ValueError, match="must match WorldState"):
        service.assign_pending(5)


# ---- _route with edge capacities ----


def _is_what_python_left(world: WorldState, digest: str) -> None:
    """The world, its records and events, as `_route` of the Python rule left them: `digest`."""
    text = json.dumps(_state(world))
    assert hashlib.sha256(text.encode()).hexdigest()[:16] == digest


def _is_what_the_phases_left(world: WorldState, digest: str) -> None:
    """The world, its records and events, as the phases' Python steps left them in the same Run:
    `digest`, recorded when they and the driver still ran it to the same state (#221)."""
    _is_what_python_left(world, digest)


def test_a_full_edge_in_a_request_records_the_wait_before_the_trip_s_sequence(
    tmp_path: Path,
) -> None:
    """`_select_toilet` goes through `_route`, in the plan of `apply_effect`."""

    controller = capacity_controller(tmp_path, "driven")
    world = controller.world
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    for edge in world.zone_edges:
        world.schedule_event(trip_event(world, edge))
    ticks = driven_ticks()
    TickEngine(world, phase_handler=controller).run(30)
    assert driven_ticks() - ticks == 30
    assert any("zone_edge_capacity_wait" in record for record in _state(world)["records"])
    _is_what_the_phases_left(world, "01da0b5553a248ed")


def test_a_trip_chosen_past_a_full_edge_takes_its_sequence_after_the_wait(tmp_path: Path) -> None:
    """One toilet behind the full edge, the others open: both happen in one proposal's plan."""

    def run(name: str) -> WorldState:
        document = staff_document(staff=[member("S01", "toilet", "support")])
        for facility in document["facilities"][1:]:
            facility["zone_id"] = "information_desk"
        document["model_configs"]["layout_routing"] = routing({"corridor_toilet": 1})
        scenario = ScenarioSpec.model_validate(document)
        world = WorldState.from_scenario(scenario)
        artifacts = RunArtifactWriter(scenario, tmp_path / name, trace_level=TraceLevel.OFF)
        artifacts.write_initial(world, execution_mode="offline")
        controller = OfflineSimulationController(world, artifacts)
        for agent in world.agents.values():
            agent.zone_id = "sleeping"
        world.schedule_event(trip_event(world, "corridor_toilet"))
        TickEngine(world, phase_handler=controller).run(40)
        return world

    ticks = driven_ticks()
    world = run("driven")
    assert driven_ticks() - ticks == 40
    driven = _state(world)
    assert any("zone_edge_capacity_wait" in record for record in driven["records"])
    assert any("agent_travel_complete" in str(event) for event in driven["pending"][1:])
    _is_what_the_phases_left(world, "c75f69288ee0e9b0")


# ---- A Run with a preemption ----


def test_an_urgent_request_preempts_a_routed_task_in_a_run(tmp_path: Path) -> None:
    """The Run no Scenario of the gate has: a priority comes from the Provider's boundary only."""

    def run(name: str) -> WorldState:
        document = staff_document(
            duration=120,
            staff=[
                member("S01", "toilet", "support", "patrol", "cleaning"),
                member("S02", "sleeping", "support", "cleaning"),
            ],
            configs={
                "staff_operations": {
                    "task_duration_minutes": {
                        "cleaning": 90,
                        "maintenance": 90,
                        "information_outreach": 90,
                        "intake": 90,
                        "patrol": 90,
                    },
                    "thresholds_by_zone": {
                        zone: {"cleaning_sanitation_below": 1.0} for zone in ("toilet", "sleeping")
                    },
                },
            },
        )
        scenario = ScenarioSpec.model_validate(document)
        world = WorldState.from_scenario(scenario)
        artifacts = RunArtifactWriter(scenario, tmp_path / name, trace_level=TraceLevel.OFF)
        artifacts.write_initial(world, execution_mode="offline")
        controller = OfflineSimulationController(world, artifacts)
        engine = TickEngine(world, phase_handler=controller)
        for _ in range(60):
            priorities = world.model_state.setdefault("llm", {}).setdefault(
                "support_priorities", {}
            )
            for request in world.unsettled_support_requests():
                priorities[request.id] = 1
            engine.step()
        return world

    ticks = driven_ticks()
    world = run("driven")
    assert driven_ticks() - ticks == 60
    assert any("staff_operation_preempted" in record for record in _state(world)["records"])
    _is_what_the_phases_left(world, "0105444ef7271471")


# What `assign_pending` left with the controller's own `_route` and `_schedule_staff_support_travel`
# as the service's route and trip, through Python, by label: `_state` taken before the kernel called
# them directly (lane P8 b3).
OWN_STEP_DIGESTS = {
    "full": "d7277550d063eaba",
    "late": "5cb01dc39695c9aa",
    "trip": "60c1b30ab87e9513",
}


@pytest.mark.parametrize(
    ("label", "reason"),
    [
        ("full", "route_unavailable_at_assignment"),
        ("late", "insufficient_run_time"),
        ("trip", None),
    ],
)
def test_the_controller_s_own_route_and_trip_decide_an_assignment(
    tmp_path: Path, label: str, reason: str | None
) -> None:
    """A full edge leaves no route (and records its wait), a task past the Run's end is not
    started, and a route with room schedules the person's trip before the task's event."""
    controller = capacity_controller(tmp_path, label)
    world = controller.world
    people = sorted(world.agents)
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    if label == "full":
        for edge in world.zone_edges:
            world.schedule_event(trip_event(world, edge))
    if label == "late":
        controller.staff_service.task_duration_minutes = world.duration_minutes
    request = pending_request(world, 1, people[0])
    assigned = controller.staff_service.assign_pending(0, validate_state=False)
    assert assigned == (() if reason else (request.id,))
    assert request.last_assignment_wait_reason_code == reason
    text = json.dumps(_state(world))
    assert hashlib.sha256(text.encode()).hexdigest()[:16] == OWN_STEP_DIGESTS[label]

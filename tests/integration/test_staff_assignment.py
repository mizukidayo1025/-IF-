"""The staff's assignment in Rust: a task, a relief, a route's wait and a preemption.

native/src/rules/staff.rs and travel.rs on what the gate reaches rarely or never
(docs/rust-port/lanes/l2-staff.md). `assign_staff_operations` and `schedule_staff_operation_relief`
have no Python body left (lane L10, `kernel_only`): what they start is checked against a digest of
what the Python rule started on the same world (its answer, the world, its records and events),
taken before it went, pinned on Linux as the gate's hashes are. No Run reaches the preemption of a
background task, so a Run here does, through the driver and through the phases.
"""

from __future__ import annotations

import hashlib
import json
import random
import sys
from pathlib import Path
from typing import Any

import pytest
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core.engine import TickEngine, TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.models import SupportRequestState, SupportRequestStatus
from sheltersim.domain.movement import shared_passage_edge_loads
from sheltersim.domain.simulator_state import initialize_simulator_extensions
from sheltersim.domain.staff import StaffService
from sheltersim.domain.staff_operations import (
    STAFF_OPERATIONS_STATE_KEY,
    assign_staff_operations,
    schedule_staff_operation_relief,
)
from sheltersim.domain.zones import ZoneGraph
from sheltersim.experiments import metrics
from sheltersim.experiments.phases.propose import ProposePhase
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.inspection.traces import RunArtifactWriter
from sheltersim.policies.space import SpaceRouter

KEY = STAFF_OPERATIONS_STATE_KEY


def _document(
    *,
    staff: list[dict[str, Any]] | None = None,
    duration: int = 200,
    configs: dict[str, Any] | None = None,
) -> dict[str, Any]:
    document = ScenarioSpec.load(SCENARIO_100_AGENTS).model_dump(mode="json")
    document["duration_minutes"] = duration
    document["scheduled_events"] = []
    if staff is not None:
        document["staff"] = staff
    document["model_configs"]["staff_operations"] = {}
    document["model_configs"]["staff_dynamics"] = {}
    document["model_configs"]["shared_operations"] = {
        "schema_version": 1,
        "policy_version": "shared-operations-v1",
    }
    document["model_configs"].update(configs or {})
    return document


def _world(**options: Any) -> WorldState:
    world = WorldState.from_scenario(ScenarioSpec.model_validate(_document(**options)))
    initialize_simulator_extensions(world)
    return world


def _routing(capacities: dict[str, int]) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "source_type": "assumption",
        "source_id": "ASM-A05-ROUTE-EDIT-V1",
        "max_agents_in_transit_by_edge": capacities,
    }


def _member(name: str, zone: str, *skills: str) -> dict[str, Any]:
    return {"id": name, "zone_id": zone, "skills": list(skills)}


def _state(world: WorldState) -> dict[str, Any]:
    return {
        "world": json.loads(world._canonical_text(world._canonical_data())),
        "records": [raw.decode() for raw in world.event_log.items_bytes(0, len(world.event_log))],
        "pending": [event.to_dict() for event in world.event_queue.pending()],
        "counters": dict(world.id_counters),
    }


def _router(world: WorldState) -> SpaceRouter:
    return SpaceRouter(ZoneGraph.from_states(world.zones, world.zone_edges))


def _zones(world: WorldState) -> list[str]:
    return sorted(world.zones)


# What the Python rules started, by the label of the call: a digest of the answer and `_state`
# after it, taken before they went (lane L10).
PYTHON_DIGESTS = {
    "ranks.None": "7da4f6ec183639af",
    "ranks.staff-urgency-first-v1": "0a361a1c16e7d532",
    "ranks.staff-travel-first-v1": "9b591b8fac3c7782",
    "zone_threshold": "3e1aecc8255f15d4",
    "rotation.0.0005": "fe1a9f6b18252347",
    "rotation.0.0015": "bf315e3139710a99",
    "rotation.0.0025": "12ac201e10b4c1df",
    "rotation.0.001": "65082333055b238e",
    "patrol_urgency.0.598": "2462696bf8fe417f",
    "patrol_urgency.0.602": "c235dab606e85b15",
    "recent_work": "d3535c084e011d31",
    "fatigue_rounds": "f4f886f4b8462b42",
    "travel_first": "b294e15f8056d14c",
    "urgency_first": "8b18793a5e14593f",
    "equal_ranks": "f3c1783863594513",
    "small_threshold": "bea27665c28fd976",
    "full_passage": "efff7d4ce6533e45",
    "passage_loads.0": "a93d4ab3da46b5fe",
    "passage_loads.1": "0f0fb0bf34700171",
    "trip_under_way": "84e9301a84f3442f",
    "relief": "708bac4aa204b371",
    "relief_least_work": "e4690399fa7ab401",
}


def _is_what_python_did(label: str, answer: Any, world: WorldState) -> None:
    text = json.dumps([list(answer), _state(world)])
    if sys.platform == "linux":
        assert hashlib.sha256(text.encode()).hexdigest()[:16] == PYTHON_DIGESTS[label]


def _assign(label: str, world: WorldState, minute: int, router: Any = None) -> Any:
    answer = assign_staff_operations(world, minute, router or _router(world))
    _is_what_python_did(label, answer, world)
    assert world._core.mirror(KEY).verify()
    return answer


def _relief(label: str, world: WorldState, minute: int) -> Any:
    answer = schedule_staff_operation_relief(world, minute)
    _is_what_python_did(label, answer, world)
    return answer


def _spread(world: WorldState, zone: str, stress: float) -> None:
    for agent in world.agents.values():
        agent.zone_id = zone
        agent.stress = stress


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


@pytest.mark.parametrize("policy", [None, "staff-urgency-first-v1", "staff-travel-first-v1"])
def test_a_worker_takes_the_task_the_python_rule_ranks_first(policy: str | None) -> None:
    configs: dict[str, Any] = {}
    if policy is not None:
        configs["policy_registry"] = {"schema_version": 2, "staff_operations": policy}

    def make() -> WorldState:
        world = _world(
            staff=[
                _member("S01", "toilet", "cleaning", "patrol", "maintenance"),
                _member("S02", "sleeping", "cleaning", "support"),
                _member("S03", "toilet", "repair"),
            ],
            configs=configs,
        )
        for zone, record in world.model_state[KEY]["zones"].items():
            record["sanitation"] = 0.5 if zone == "toilet" else 0.7
        _spread(world, "sleeping", 0.8)
        facility = sorted(world.facilities)[0]
        world.facilities[facility].working_units = world.facilities[facility].capacity - 1
        return world

    first = make()
    answer = _assign(f"ranks.{policy}", first, 0)
    assert len(answer) == 2
    kinds = {record["task_kind"] for record in first.model_state[KEY]["staff"].values()}
    assert len(kinds) >= 2


def test_a_zone_s_threshold_names_the_reason_as_python_formats_it() -> None:
    thresholds = {"toilet": {"cleaning_sanitation_below": 1}}

    def make() -> WorldState:
        world = _world(
            staff=[_member("S01", "toilet", "cleaning")],
            configs={"staff_operations": {"thresholds_by_zone": thresholds}},
        )
        world.model_state[KEY]["zones"]["toilet"]["sanitation"] = 0.99
        return world

    first = make()
    assert _assign("zone_threshold", first, 0)
    assert first.model_state[KEY]["staff"]["S01"]["reason"] == "sanitation_index_below_1"


def test_the_rotation_reads_the_history_and_the_fatigue() -> None:
    def make(fatigue: float) -> WorldState:
        world = _world(staff=[_member(f"S{n}", "toilet", "patrol") for n in range(3)])
        _spread(world, "toilet", 0.9)
        state = world.model_state["staff_dynamics_v1"]["staff"]
        state["S0"]["fatigue"] = fatigue
        state["S1"]["fatigue"] = 0.0005
        return world

    # 1000 * 0.0005 is half way: round() goes to even, and the ties go by ID.
    for fatigue in (0.0005, 0.0015, 0.0025, 0.001):
        answer = _assign(f"rotation.{fatigue}", make(fatigue), 0)
        assert len(answer) == 1


def test_a_patrol_s_urgency_is_half_the_mean_stress_against_another_task() -> None:
    """Either side of the rank: a 1% change of the factor swaps them."""

    def make(sanitation: float) -> WorldState:
        world = _world(staff=[_member("S01", "toilet", "cleaning", "patrol")])
        _spread(world, "toilet", 0.8)
        for zone, record in world.model_state[KEY]["zones"].items():
            record["sanitation"] = sanitation if zone == "toilet" else 1.0
        return world

    for sanitation, kind in ((0.598, "cleaning"), (0.602, "patrol")):
        first = make(sanitation)
        assert _assign(f"patrol_urgency.{sanitation}", first, 0)
        assert first.model_state[KEY]["staff"]["S01"]["task_kind"] == kind


def test_the_rotation_counts_recent_work_from_four_hours_back() -> None:
    def make() -> WorldState:
        world = _world(staff=[_member(f"S{n}", "toilet", "patrol") for n in range(2)], duration=400)
        _spread(world, "toilet", 0.9)
        world.minute_index = 300
        entry = {
            "task_id": "old",
            "task_kind": "patrol",
            "target_id": None,
            "target_zone_id": "toilet",
            "reason": "r",
            "status": "completed",
            "start_minute": 0,
            "arrival_minute": 0,
            "travel_minutes": 0,
            "causation_event_ids": [],
        }
        history = world.model_state[KEY]["history"]
        history.append({**entry, "staff_id": "S0", "complete_minute": 60})
        history.append({**entry, "staff_id": "S1", "complete_minute": 59})
        return world

    first = make()
    assert _assign("recent_work", first, 300)
    state = first.model_state[KEY]["staff"]
    assert state["S1"]["phase"] != "idle" and state["S0"]["phase"] == "idle"


def test_a_fatigue_rounds_at_a_thousandth_in_the_rotation() -> None:
    def make() -> WorldState:
        world = _world(staff=[_member(f"S{n}", "toilet", "patrol") for n in range(2)])
        _spread(world, "toilet", 0.9)
        state = world.model_state["staff_dynamics_v1"]["staff"]
        state["S0"]["fatigue"] = 0.0504
        state["S1"]["fatigue"] = 0.0498
        return world

    first = make()
    assert _assign("fatigue_rounds", first, 0)
    # Both round to 50, so the ID decides; 1010 * fatigue would give 51 and 50.
    assert first.model_state[KEY]["staff"]["S0"]["phase"] != "idle"


def test_the_travel_first_formula_takes_the_nearer_task() -> None:
    def make(policy: str) -> WorldState:
        world = _world(
            staff=[_member("S01", "toilet", "cleaning")],
            configs={"policy_registry": {"schema_version": 2, "staff_operations": policy}},
        )
        for zone, record in world.model_state[KEY]["zones"].items():
            record["sanitation"] = 0.8 if zone == "toilet" else 0.2
        return world

    near = make("staff-travel-first-v1")
    assert _assign("travel_first", near, 0)
    assert near.model_state[KEY]["staff"]["S01"]["target_zone_id"] == "toilet"
    urgent = make("staff-urgency-first-v1")
    assert _assign("urgency_first", urgent, 0)
    assert urgent.model_state[KEY]["staff"]["S01"]["target_zone_id"] != "toilet"


def test_equal_ranks_go_by_the_kind_as_python_compares_them() -> None:
    def make() -> WorldState:
        world = _world(staff=[_member("S01", "toilet", "cleaning", "patrol")])
        _spread(world, "toilet", 0.8)
        for zone, record in world.model_state[KEY]["zones"].items():
            record["sanitation"] = 0.6 if zone == "toilet" else 1.0
        return world

    assert 1.0 - 0.6 == 0.8 * 0.5
    first = make()
    assert _assign("equal_ranks", first, 0)
    assert first.model_state[KEY]["staff"]["S01"]["task_kind"] == "patrol"


def test_a_small_threshold_names_the_reason_with_an_exponent() -> None:
    def make() -> WorldState:
        world = _world(
            staff=[_member("S01", "toilet", "cleaning")],
            configs={
                "staff_operations": {
                    "thresholds_by_zone": {"toilet": {"cleaning_sanitation_below": 0.00001}}
                }
            },
        )
        for zone, record in world.model_state[KEY]["zones"].items():
            record["sanitation"] = 0.0 if zone == "toilet" else 1.0
        return world

    first = make()
    assert _assign("small_threshold", first, 0)
    assert first.model_state[KEY]["staff"]["S01"]["reason"] == "sanitation_index_below_1e-05"


def test_a_full_passage_keeps_a_worker_from_a_task() -> None:
    capacities = {"layout_routing": _routing({})}

    def make() -> tuple[WorldState, SpaceRouter]:
        world = _world(
            staff=[_member("S01", "toilet", "patrol"), _member("S02", "toilet", "patrol")],
            configs=capacities,
        )
        _spread(world, "sleeping", 0.9)
        graph = ZoneGraph.from_states(
            world.zones, world.zone_edges, edge_capacities={edge: 1 for edge in world.zone_edges}
        )
        return world, SpaceRouter(graph)

    first, first_router = make()
    answer = _assign("full_passage", first, 0, first_router)
    assert len(answer) == 1


def test_an_unknown_formula_and_a_corrupt_value_are_refused() -> None:
    """A fatigue no rule writes (the Python rule's `round` raised OverflowError on it) and a formula
    the kernel doesn't know: KernelRefusal, with nothing changed."""
    world = _world(staff=[_member("S01", "toilet", "patrol")])
    _spread(world, "toilet", 0.9)
    before = json.dumps(_state(world))
    world.model_state["staff_dynamics_v1"]["staff"]["S01"]["fatigue"] = float("inf")
    with pytest.raises(native.Unsupported):
        native.rules("staff").assign_operations(world, 0, _router(world))
    with pytest.raises(KernelRefusal):
        assign_staff_operations(world, 0, _router(world))
    world.model_state["staff_dynamics_v1"]["staff"]["S01"]["fatigue"] = 0.0
    assert json.dumps(_state(world)) == before
    world.model_configs["policy_registry"] = {"staff_operations": "a-later-formula"}
    with pytest.raises(KernelRefusal):
        assign_staff_operations(world, 0, _router(world))


def test_the_passage_loads_are_the_python_rule_s() -> None:
    """`shared_passage_edge_loads` through what `assign_staff_operations` starts on them."""

    def make() -> WorldState:
        world = _world(staff=[_member(f"S{n}", "toilet", "patrol") for n in range(3)])
        _spread(world, "sleeping", 0.9)
        return world

    first = make()
    _assign("passage_loads.0", first, 0)
    assert shared_passage_edge_loads(first, 1)
    first.minute_index = 1
    # The zones stay reserved, so a later minute starts nothing new on them and reads the loads.
    _assign("passage_loads.1", first, 1)


def test_a_trip_under_way_loads_the_edge_its_share_of_the_time_has_reached() -> None:
    """At 496 of 1000 minutes the walker is on the first edge; a 1% scale would say the second."""

    def make() -> tuple[WorldState, SpaceRouter]:
        world = _world(
            duration=3000,
            staff=[_member("S01", "corridor", "patrol"), _member("S09", "sleeping", "cleaning")],
        )
        _spread(world, "toilet", 0.9)
        for edge in ("sleeping_corridor", "corridor_toilet"):
            world.zone_edges[edge].travel_minutes = 500
        world.minute_index = 496
        world.staff["S09"].active_task_id = "walking"
        state = world.model_state[KEY]["staff"]
        state["S09"] = {
            **state["S09"],
            "phase": "travel",
            "task_id": "walking",
            "task_kind": "cleaning",
            "target_zone_id": "toilet",
            "reason": "r",
            "started_minute": 0,
            "travel_due_minute": 1000,
            "due_minute": 1010,
            "route_zone_ids": ["sleeping", "corridor", "toilet"],
            "route_edge_ids": ["sleeping_corridor", "corridor_toilet"],
            "travel_minutes": 1000,
        }
        graph = ZoneGraph.from_states(
            world.zones, world.zone_edges, edge_capacities={"corridor_toilet": 1}
        )
        return world, SpaceRouter(graph)

    first, first_router = make()
    assert shared_passage_edge_loads(first, 496) == {"sleeping_corridor": 1}
    assert _assign("trip_under_way", first, 496, first_router)


# ---- schedule_staff_operation_relief ----


def _on_a_task() -> WorldState:
    world = _world(
        duration=40,
        staff=[
            _member("S01", "toilet", "cleaning"),
            _member("S02", "toilet", "sanitation"),
            _member("S03", "toilet", "sanitation"),
            _member("S04", "sleeping", "cleaning"),
        ],
        configs={
            "staff_dynamics": {
                "shift_windows_by_staff": {
                    "S01": [{"start_minute": 0, "end_minute": 2}],
                    "S02": [{"start_minute": 2, "end_minute": 40}],
                    "S03": [{"start_minute": 2, "end_minute": 40}],
                    "S04": [{"start_minute": 2, "end_minute": 40}],
                }
            },
            "staff_operations": {
                "task_duration_minutes": {
                    "cleaning": 8,
                    "maintenance": 8,
                    "information_outreach": 8,
                    "intake": 8,
                    "patrol": 8,
                }
            },
        },
    )
    world.model_state[KEY]["zones"]["toilet"]["sanitation"] = 0.5
    assert assign_staff_operations(world, 0, _router(world))
    return world


def test_a_relief_goes_to_the_replacement_the_python_rule_picks() -> None:
    first = _on_a_task()
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


def test_a_relief_goes_to_the_replacement_with_the_least_work() -> None:
    def make() -> WorldState:
        world = _on_a_task()
        world.minute_index = 1
        world.model_state["staff_dynamics_v1"]["staff"]["S02"]["fatigue"] = 0.5
        return world

    first = make()
    _relief("relief_least_work", first, 1)
    assert (
        next(
            event.payload["replacement_staff_id"]
            for event in first.event_queue.pending()
            if event.event_type == "staff_operation_relief"
        )
        == "S03"
    )


def test_no_relief_for_a_worker_still_on_shift_or_too_near_the_end() -> None:
    world = _on_a_task()
    before = json.dumps(_state(world))
    assert schedule_staff_operation_relief(world, 0) == ()
    world.model_state[KEY]["staff"]["S01"]["due_minute"] = 2
    world.minute_index = 1
    assert schedule_staff_operation_relief(world, 1) == ()
    assert json.loads(json.dumps(_state(world)))["pending"] == json.loads(before)["pending"]


# ---- What the metrics read, with routes ----
#
# `_support_wait_snapshot` has no Python rule left (lane L10, `kernel_only`). The reasons below
# are the Python rule's, and so is the digest of the many worlds' answers, recorded from it.


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


def _pending(world: WorldState, number: int, agent_id: str, minute: int = 0) -> SupportRequestState:
    request = SupportRequestState(
        id=f"support-{number}",
        agent_id=agent_id,
        requested_minute=minute,
        timeout_minute=minute + 90,
    )
    world.support_requests[request.id] = request
    return request


def _reasons(world: WorldState, minute: int = 0) -> list[list[str]]:
    return [detail["reason_codes"] for detail in _both_snapshots(world, minute).values()]


def test_the_reasons_a_free_worker_s_route_gives_are_the_python_rule_s() -> None:
    world = _world(staff=[_member("S01", "toilet", "support")], duration=200)
    people = sorted(world.agents)
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    _pending(world, 1, people[0])
    assert _reasons(world) == [["assignment_not_attempted_at_sampled_minute"]]
    world.metric_state["support_assignment_metrics_minute"] = 0.0
    assert _reasons(world) == [["assignment_failure_reason_not_recorded"]]
    request = world.support_requests["support-1"]
    request.last_assignment_attempt_minute = 0
    assert _reasons(world) == [["assignment_failure_reason_not_recorded"]]
    request.last_assignment_wait_reason_code = "route_unavailable_at_assignment"
    assert _reasons(world) == [["route_unavailable_at_assignment"]]


def test_a_route_past_the_end_of_the_run_is_its_reason() -> None:
    world = _world(staff=[_member("S01", "toilet", "support")], duration=3)
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    _pending(world, 1, sorted(world.agents)[0])
    assert _reasons(world) == [["insufficient_run_time"]]


def test_a_route_that_ends_exactly_at_the_run_s_end_is_too_late() -> None:
    plain = _world(staff=[_member("S01", "toilet", "support")])
    route = _router(plain).route("sleeping", "toilet")
    assert route is not None
    # Its minutes, and ten of service.
    last = route.travel_minutes + 10
    for duration, too_late in ((last, True), (last + 1, False)):
        world = _world(staff=[_member("S01", "toilet", "support")], duration=duration)
        for agent in world.agents.values():
            agent.zone_id = "sleeping"
        _pending(world, 1, sorted(world.agents)[0])
        assert ("insufficient_run_time" in _reasons(world)[0]) is too_late


def test_a_full_destination_counts_who_is_on_the_way() -> None:
    from sheltersim.domain.models import Activity

    world = _world(staff=[_member("S01", "toilet", "support")])
    people = sorted(world.agents)
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    world.agents[people[1]].zone_id = "toilet"
    world.agents[people[2]].activity = Activity.MOVING
    world.agents[people[2]].target_id = "toilet"
    _pending(world, 1, people[0])
    world.zones["toilet"].capacity = 2
    assert _reasons(world) == [["staff_destination_full"]]
    world.zones["toilet"].capacity = 3
    assert _reasons(world) != [["staff_destination_full"]]


def test_a_full_destination_and_a_full_passage_are_their_reasons() -> None:
    world = _world(staff=[_member("S01", "toilet", "support")])
    people = sorted(world.agents)
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    _pending(world, 1, people[0])
    world.zones["toilet"].capacity = 0
    assert _reasons(world) == [["staff_destination_full"]]
    world.zones["toilet"].capacity = None
    world.model_configs["layout_routing"] = _routing({edge: 1 for edge in world.zone_edges})
    state = world.model_state[KEY]["staff"]
    for edge in world.zone_edges.values():
        world.schedule_event(_trip(world, edge.id))
    assert state
    assert _reasons(world) == [["route_capacity_full"]]
    for edge in world.zone_edges.values():
        edge.closed = True
    assert _reasons(world) == [["no_route_to_qualified_staff"]]


def _trip(world: WorldState, edge_id: str) -> Any:
    from sheltersim.events.models import EventKind, ScheduledEvent

    sequence = world.allocate_event_sequence()
    return ScheduledEvent(
        event_id=f"agent-travel-complete-{sequence}",
        kind=EventKind.COMPLETION,
        due_minute=world.duration_minutes - 1,
        priority=0,
        sequence=sequence,
        event_type="agent_travel_complete",
        payload={"agent_id": "nobody", "task_id": "none", "route_edge_ids": [edge_id]},
        source_id="test",
        provenance={"source_type": "assumption", "source_id": "test"},
    )


def test_many_worlds_read_the_same_reasons_with_free_and_busy_workers() -> None:
    generator = random.Random(2615)
    seen: set[str] = set()
    answers: list[str] = []
    for _ in range(120):
        world = _world(
            staff=[
                _member("S01", generator.choice(["toilet", "sleeping"]), "support"),
                _member("S02", generator.choice(["toilet", "sleeping"]), "medical"),
                _member("S03", generator.choice(["toilet", "sleeping"]), "support", "nursing"),
            ],
            duration=generator.choice([3, 5, 200]),
        )
        for agent in world.agents.values():
            agent.zone_id = generator.choice(["toilet", "sleeping"])
        for staff in world.staff.values():
            if generator.random() < 0.4:
                staff.active_task_id = "busy"
        people = sorted(world.agents)
        for number in range(4):
            request = _pending(world, number, people[number])
            if generator.random() < 0.3:
                request.last_assignment_attempt_minute = 0
                request.last_assignment_wait_reason_code = generator.choice(
                    [None, "", "insufficient_run_time"]
                )
        if generator.random() < 0.5:
            world.metric_state["support_assignment_metrics_minute"] = 0.0
        if generator.random() < 0.3:
            world.zones[generator.choice(_zones(world))].capacity = generator.choice([0, 1, 300])
        answers.append(json.dumps(_both_snapshots(world, 0)))
        for reasons in _reasons(world):
            seen.update(reasons)
    digest = hashlib.sha256("\n".join(answers).encode("utf-8")).hexdigest()[:16]
    assert digest == "886f522f9c88ebae"
    assert {
        "staff_busy",
        "assignment_not_attempted_at_sampled_minute",
        "assignment_failure_reason_not_recorded",
        "insufficient_run_time",
        "staff_destination_full",
    } <= seen


# ---- assign_pending ----


def test_a_free_worker_or_a_priority_hands_the_step_to_assign_work() -> None:
    world = _world(staff=[_member("S01", "toilet", "support")])
    request = _pending(world, 1, sorted(world.agents)[0])
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
    world = _world(staff=[_member("S01", "toilet", "support")])
    service = StaffService(world)
    with pytest.raises(ValueError, match="must match WorldState"):
        service.assign_pending(5)


# ---- _route with edge capacities ----


def _capacity_controller(
    tmp_path: Path, name: str, capacity: int = 1
) -> OfflineSimulationController:
    document = _document(staff=[_member("S01", "toilet", "support")])
    edges = [edge["id"] for edge in document["zone_edges"]]
    document["model_configs"]["layout_routing"] = _routing({edge: capacity for edge in edges})
    scenario = ScenarioSpec.model_validate(document)
    world = WorldState.from_scenario(scenario)
    artifacts = RunArtifactWriter(scenario, tmp_path / name, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    return OfflineSimulationController(world, artifacts)


def test_a_route_with_a_full_edge_records_its_wait_as_the_python_rule_does(tmp_path: Path) -> None:
    first = _capacity_controller(tmp_path, "first")
    second = _capacity_controller(tmp_path, "second")
    people = sorted(first.world.agents)
    route = native.rules("travel").route
    for controller in (first, second):
        for agent in controller.world.agents.values():
            agent.zone_id = "sleeping"
        for edge in controller.world.zone_edges:
            controller.world.schedule_event(_trip(controller.world, edge))
    answer = route(first, "sleeping", "toilet", people[0])
    expected = ProposePhase._route(second, "sleeping", "toilet", people[0])
    assert answer == expected is None
    answer = route(first, "sleeping", "sleeping", people[0])
    assert answer == ProposePhase._route(second, "sleeping", "sleeping", people[0])
    assert answer.edge_ids == ()
    assert json.dumps(_state(first.world)) == json.dumps(_state(second.world))
    assert [
        record.event.event_type
        for record in first.world.event_log.records
        if record.event.event_type == "zone_edge_capacity_wait"
    ] == ["zone_edge_capacity_wait"]


def test_a_route_with_room_on_its_edges_counts_the_trips_under_way(tmp_path: Path) -> None:
    first = _capacity_controller(tmp_path, "first", capacity=2)
    second = _capacity_controller(tmp_path, "second", capacity=2)
    people = sorted(first.world.agents)
    for controller in (first, second):
        for edge in controller.world.zone_edges:
            controller.world.schedule_event(_trip(controller.world, edge))
    answer = native.rules("travel").route(first, "sleeping", "toilet", people[0])
    assert answer == ProposePhase._route(second, "sleeping", "toilet", people[0])
    assert answer is not None and answer.edge_ids
    assert json.dumps(_state(first.world)) == json.dumps(_state(second.world))


def test_a_full_edge_in_a_request_records_the_wait_before_the_trip_s_sequence(
    tmp_path: Path,
) -> None:
    """`_select_toilet` goes through `_route`, in the plan of `apply_effect`."""

    def run(name: str, *, on_the_phases: bool) -> dict[str, Any]:
        controller = _capacity_controller(tmp_path, name)
        world = controller.world
        for agent in world.agents.values():
            agent.zone_id = "sleeping"
        for edge in world.zone_edges:
            world.schedule_event(_trip(world, edge))
        native.rules("tick").python_path_only(on_the_phases)
        try:
            engine = TickEngine(world, phase_handler=controller)
            engine.run(30)
        finally:
            native.rules("tick").python_path_only(False)
        return _state(world)

    driven = run("driven", on_the_phases=False)
    phases = run("phases", on_the_phases=True)
    assert json.dumps(driven) == json.dumps(phases)
    assert any("zone_edge_capacity_wait" in record for record in driven["records"])


def test_a_trip_chosen_past_a_full_edge_takes_its_sequence_after_the_wait(tmp_path: Path) -> None:
    """One toilet behind the full edge, the others open: both happen in one proposal's plan."""

    def run(name: str, *, on_the_phases: bool) -> dict[str, Any]:
        document = _document(staff=[_member("S01", "toilet", "support")])
        for facility in document["facilities"][1:]:
            facility["zone_id"] = "information_desk"
        document["model_configs"]["layout_routing"] = _routing({"corridor_toilet": 1})
        scenario = ScenarioSpec.model_validate(document)
        world = WorldState.from_scenario(scenario)
        artifacts = RunArtifactWriter(scenario, tmp_path / name, trace_level=TraceLevel.OFF)
        artifacts.write_initial(world, execution_mode="offline")
        controller = OfflineSimulationController(world, artifacts)
        for agent in world.agents.values():
            agent.zone_id = "sleeping"
        world.schedule_event(_trip(world, "corridor_toilet"))
        native.rules("tick").python_path_only(on_the_phases)
        try:
            TickEngine(world, phase_handler=controller).run(40)
        finally:
            native.rules("tick").python_path_only(False)
        return _state(world)

    driven = run("driven", on_the_phases=False)
    assert json.dumps(driven) == json.dumps(run("phases", on_the_phases=True))
    assert any("zone_edge_capacity_wait" in record for record in driven["records"])
    assert any("agent_travel_complete" in str(event) for event in driven["pending"][1:])


# ---- A Run with a preemption ----


def test_an_urgent_request_preempts_a_routed_task_in_a_run(tmp_path: Path) -> None:
    """The Run no Scenario of the gate has: a priority comes from the Provider's boundary only."""

    def run(name: str, *, on_the_phases: bool) -> dict[str, Any]:
        document = _document(
            duration=120,
            staff=[
                _member("S01", "toilet", "support", "patrol", "cleaning"),
                _member("S02", "sleeping", "support", "cleaning"),
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
        native.rules("tick").python_path_only(on_the_phases)
        try:
            for _ in range(60):
                priorities = world.model_state.setdefault("llm", {}).setdefault(
                    "support_priorities", {}
                )
                for request in world.unsettled_support_requests():
                    priorities[request.id] = 1
                engine.step()
        finally:
            native.rules("tick").python_path_only(False)
        return _state(world)

    counts = native.rules("tick").stats()[0]
    ticks = counts["ticks"]
    driven = run("driven", on_the_phases=False)
    assert native.rules("tick").stats()[0]["ticks"] == ticks + 60
    phases = run("phases", on_the_phases=True)
    assert json.dumps(driven) == json.dumps(phases)
    assert any("staff_operation_preempted" in record for record in driven["records"])

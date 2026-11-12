"""movement_v1 owned by Rust (docs/rust-port/lanes/i-movement.md, m4-movement.md).

The row against the tree at every tick of the gate is benchmarks/rule_shadow/movement_shadow.py;
here, the shapes of its records, what taints it, and the steps that hold the tree. The kernels are
tests/integration/test_movement_rules.py; what depends on the order of the walk,
tests/integration/test_movement_walk_order.py.
"""

from __future__ import annotations

import copy
import json
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
from sheltersim.domain import movement
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
from sheltersim.domain.movement import validate_movement_runtime_state
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink


def _position(point: dict[str, float] | None = None) -> dict[str, Any]:
    return {
        "zone_id": "hall",
        "point": point,
        "edge_id": None,
        "progress": 0.0,
        "position_kind": "physical" if point else "unavailable",
    }


def _journey(**more: Any) -> dict[str, Any]:
    return {
        "event_id": "e1",
        "task_id": "t1",
        "purpose": "walk",
        "facility_id": None,
        "destination_zone_id": "hall",
        "route_zone_ids": ["hall", "yard"],
        "route_edge_ids": ["edge"],
        "route_points": [],
        "started_minute": 1,
        "due_minute": 3,
        **more,
    }


def _state() -> dict[str, Any]:
    return {
        "schema_version": 2,
        "policy_version": movement.MOVEMENT_POLICY_VERSION,
        "positions": {"b": _position({"x": 1.5, "y": -2.0}), "a": _position()},
        "staff_positions": {"s": _position()},
        "journeys": {
            "b": _journey(queue_id="q", route_points=[{"x": 0.0, "y": 0.0}, {"x": 1.0, "y": 1.0}]),
            "a": _journey(support_request_id="r", support_staff_id="s"),
            "c": _journey(facility_id="f"),
        },
        "edge_usage_minutes": {"edge": 3},
        "last_updated_minute": 0,
    }


def _world(state: dict[str, Any] | None = None) -> WorldState:
    return WorldState("s", 1, 100, model_state={KEY: state if state is not None else _state()})


def _mirror(world: WorldState) -> Any:
    return world._core.mirror(KEY)


def _run(scenario: Any, minutes: int, check: Any = None) -> WorldState:
    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
        if check is not None:
            check(world)
    return world


# ---- The row -----------------------------------------------------------------------------------


def test_the_row_writes_the_bytes_of_the_tree_for_every_shape_of_a_journey() -> None:
    world = _world()
    assert _mirror(world).tainted is None
    assert _mirror(world).row_json() == native.canonical_json(_state())
    assert _mirror(world).verify()
    assert list(world.model_state[KEY]["positions"]) == ["b", "a"]


@pytest.mark.parametrize(
    "write",
    [
        lambda state: state["positions"]["a"].__setitem__("zone_id", "yard"),
        lambda state: state["positions"]["a"].__setitem__("point", {"x": 1.0, "y": 2.0}),
        lambda state: state["positions"]["b"].__setitem__("point", None),
        lambda state: state["positions"]["b"]["point"].__setitem__("x", 9.5),
        lambda state: state["positions"]["b"].update(edge_id="edge", progress=0.5),
        lambda state: state["positions"].__setitem__("n", _position({"x": 0.0, "y": 0.0})),
        lambda state: state["positions"].pop("a"),
        lambda state: state["staff_positions"].__setitem__("s", _position({"x": 1.0, "y": 1.0})),
        lambda state: state["journeys"].__setitem__("n", _journey(queue_id="q")),
        lambda state: state["journeys"].pop("b"),
        lambda state: state["journeys"]["a"].__setitem__("due_minute", 9),
        lambda state: state["journeys"]["c"].__setitem__("queue_id", "q"),
        lambda state: state["journeys"]["b"].pop("queue_id"),
        lambda state: state["journeys"]["a"]["route_zone_ids"].append("yard"),
        lambda state: state["journeys"]["b"]["route_points"][0].__setitem__("y", 4.0),
        lambda state: state["edge_usage_minutes"].__setitem__(
            "edge", state["edge_usage_minutes"].get("edge", 0) + 1
        ),
        lambda state: state.__setitem__("last_updated_minute", 7),
    ],
)
def test_a_forwarded_write_reaches_the_row(write: Any) -> None:
    world = _world()
    write(world.model_state[KEY])
    assert _mirror(world).tainted is None
    assert _mirror(world).verify()
    assert _mirror(world).row_json() == native.canonical_json(
        json.loads(json.dumps(world.model_state[KEY]))
    )


@pytest.mark.parametrize(
    "write",
    [
        lambda state: state["positions"]["a"].__setitem__("progress", 1),
        lambda state: state["positions"]["b"]["point"].__setitem__("x", 1),
        lambda state: state["positions"]["b"]["point"].__setitem__("z", 1.0),
        lambda state: state["positions"]["a"].__setitem__("other", 1),
        lambda state: state["positions"]["a"].pop("edge_id"),
        lambda state: state["positions"].__setitem__("n", {"zone_id": "hall"}),
        lambda state: state["journeys"]["a"].__setitem__("other", 1),
        lambda state: state["journeys"]["a"].pop("event_id"),
        lambda state: state["journeys"]["a"].__setitem__("queue_id", None),
        lambda state: state["journeys"].__setitem__("n", _journey(other=1)),
        lambda state: state["journeys"].__setitem__("n", _journey(queue_id=None)),
        lambda state: state["journeys"]["a"].__setitem__("started_minute", True),
        lambda state: state["edge_usage_minutes"].__setitem__("edge", 1.0),
        lambda state: state.__setitem__("other", 1),
    ],
)
def test_a_value_of_another_type_taints_the_row_and_the_tree_keeps_it(write: Any) -> None:
    world = _world()
    write(world.model_state[KEY])
    assert _mirror(world).tainted is not None
    # Nothing else depends on the row yet, and the tree is what the rules read.
    assert world.model_state[KEY] is not None
    assert native.canonical_json(world.model_state[KEY])


def test_a_bad_value_gets_the_python_rules_error() -> None:
    world = _world()
    world.model_state[KEY]["schema_version"] = "2"
    assert _mirror(world).tainted is not None
    with pytest.raises(ValueError, match="movement_v1 has an invalid exact schema"):
        validate_movement_runtime_state(world)


def test_the_int_progress_validation_accepts_runs_on_a_tainted_row() -> None:
    state = _state()
    state["positions"]["a"]["progress"] = 1
    world = _world(state)
    assert _mirror(world).tainted is not None
    assert world.model_state[KEY]["positions"]["a"]["progress"] == 1


# ---- With the rules ----------------------------------------------------------------------------


def test_the_row_follows_the_rules_with_only_single_records_written(
    generated_scenario: Any,
) -> None:
    def check(world: WorldState) -> None:
        assert _mirror(world).tainted is None
        assert _mirror(world).verify()

    tracked = _run(generated_scenario, 60, check)
    stats = _mirror(tracked).stats()
    # Written once when the namespace is made; everything after is a record at a time.
    assert stats["whole_writes"] <= 1
    assert tracked.model_state[KEY]["positions"]


def test_the_step_holds_the_tree_across_advance_movement_and_its_writes_reach_the_row(
    generated_scenario: Any,
) -> None:
    world = _run(generated_scenario, 5)
    held = world.model_state[KEY]
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    agent_id = next(iter(held["journeys"]), None) or next(iter(world.agents))
    held["journeys"].setdefault(
        agent_id,
        _journey(
            route_zone_ids=[world.agents[agent_id].zone_id],
            route_edge_ids=[],
            destination_zone_id=world.agents[agent_id].zone_id,
        ),
    )
    # A replan: the step reads and pops through the name it took before advance_movement ran.
    controller._replan_journeys(held, (agent_id,), world.minute_index)
    assert world.model_state[KEY] is held
    assert _mirror(world).tainted is None
    assert _mirror(world).verify()
    assert world.model_state[KEY]["journeys"] is held["journeys"]


def test_restore_and_copy_read_the_row_again(generated_scenario: Any) -> None:
    world = _run(generated_scenario, 10)
    expected = world.canonical_json()
    restored = WorldState.from_dict(json.loads(json.dumps(world.to_dict())))
    assert restored.canonical_json() == expected
    assert _mirror(restored).tainted is None
    assert _mirror(restored).verify()
    # As storage copies a restored world (_copy_restored): scheduled events are frozen and shared.
    copied = copy.deepcopy(restored, {id(event): event for event in restored.event_queue.pending()})
    assert copied.canonical_json() == expected
    assert _mirror(copied).tainted is None
    assert _mirror(copied).verify()

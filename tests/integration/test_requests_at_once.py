"""The Python entries of `_apply_effect` with no trip to make, and of the checks of having arrived.

The requests at once and the requests closed at once, and the core's answers of having arrived,
are tested in the core without Python (native/core/tests/requests_at_once.rs, Issue #225), on the
worlds this file built (`tests/support/core_fixtures_service.py`). This file keeps the boundary: a
manager's `request` a test patched, the information model's tree that follows its row, a journey
the PyO3 entry reads as none or refuses, the staff positions it gives as Python objects, and the
answers of a world without a floor plan or movement state, which the PyO3 entries give themselves.
"""

from __future__ import annotations

import copy
import json
from types import SimpleNamespace
from typing import Any

import pytest
from support.core_fixtures_service import facility_of, place_at
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain import movement
from sheltersim.domain.information import INFORMATION_SPREAD_STATE_KEY as INFORMATION
from sheltersim.domain.information import create_information_runtime_state
from sheltersim.domain.models import InformationSpreadConfig
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.behavior import ActionKind
from sheltersim.policies.information import InformationSpreadPolicy
from sheltersim.policies.space import ZoneRoute


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _proposal(action: ActionKind, agent_id: str, world: WorldState) -> Any:
    return SimpleNamespace(
        action=action, agent_id=agent_id, next_decision_at=world.minute_index + 7, target_id=None
    )


def _without_a_floor_plan() -> WorldState:
    scenario = ScenarioSpec.model_validate_json(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    world = WorldState.from_scenario(scenario)
    assert "facility_geometry" not in world.model_configs
    return world


# ---- A queue at once -----------------------------------------------------------------------------


def test_a_toilet_request_a_test_patched_is_a_kernel_refusal(
    generated_scenario_with_split_queues: Any,
) -> None:
    """A manager's own `request` is not the queue's kernel, and the step has no Python body."""
    world = WorldState.from_scenario(generated_scenario_with_split_queues)
    place_at(world, "A001", "toilet_01")
    controller = _controller(world)
    controller.toilet_service.request = lambda *_arguments, **_options: None
    before = _bytes(world)
    with pytest.raises(KernelRefusal, match="another function"):
        controller._apply_effect(_proposal(ActionKind.REQUEST_TOILET, "A001", world))
    assert _bytes(world) == before


# ---- A request closed at once --------------------------------------------------------------------


# ---- The information model's record of a request ------------------------------------------------


def _heard(message: str, belief: str) -> dict[str, Any]:
    return {
        "message_id": message,
        "belief": belief,
        "source": "SRC",
        "last_contact": 0,
        "last_event_id": f"event-{message}",
        "exposed": True,
        "belief_strength": 0.5,
        "share_propensity": 0.5,
        "correction_exposed": False,
        "correction_accepted": False,
        "ever_misbelieved": False,
        "ever_forgotten": False,
        "last_correction_accepted": False,
    }


def test_a_request_records_the_heard_claims_on_the_tree_as_the_python_rule_does(
    generated_scenario: Any,
) -> None:
    """The core writes the record on the row of `information_spread_v1` (lane P8 e1, #152): the
    attached tree follows it, the same objects with the Python rule's values and key order."""
    world = WorldState.from_scenario(generated_scenario)
    state = create_information_runtime_state(set(world.agents))
    for message, belief in (("m2", "heard"), ("m3", "believed"), ("m1", "heard")):
        state["beliefs"]["A001"][message] = _heard(message, belief)
    state["query_pressure"]["A001"] = 0.75
    world.model_state[INFORMATION] = state
    tree = world.model_state[INFORMATION]
    expected = copy.deepcopy(tree)
    controller = _controller(world)
    controller.information_policy = InformationSpreadPolicy(InformationSpreadConfig(enabled=True))
    target, _, committed = native.rules("proposals").apply_effect(
        controller, _proposal(ActionKind.SEEK_INFORMATION, "A001", world)
    )
    assert committed is True
    # What the deleted Python `record_support_request` wrote: the request tied to the heard claims
    # in ID order, the counter, and the person's query pressure back to 0.0.
    expected["support_requests"].append(
        {
            "request_id": target,
            "agent_id": "A001",
            "minute": world.minute_index,
            "message_ids": ["m1", "m2"],
        }
    )
    expected["counters"]["support_requests"] += 1
    expected["query_pressure"]["A001"] = 0.0
    assert world.model_state[INFORMATION] is tree
    assert json.dumps(tree) == json.dumps(expected)
    assert tree["support_requests"][-1]["message_ids"] == ["m1", "m2"]
    # Someone with no heard claim: nothing changes.
    before = json.dumps(tree)
    native.rules("proposals").apply_effect(
        controller, _proposal(ActionKind.SEEK_INFORMATION, "A002", world)
    )
    assert json.dumps(tree) == before


# ---- Having arrived ------------------------------------------------------------------------------


def _answer(world: WorldState, name: str, *arguments: Any) -> bool:
    """The kernel's answer; the cases below list the deleted Python rule's, recorded from it."""
    before = _bytes(world)
    answer = getattr(movement, name)(world, *arguments)
    assert _bytes(world) == before
    return answer


def test_having_arrived_at_the_end_of_a_trip(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    state = world.model_state[KEY]
    target = {"x": 1.5, "y": 2.5}
    journey = {"route_points": [{"x": 0.5, "y": 0.5}, target]}
    state["positions"]["A001"]["point"] = dict(target)
    assert _answer(world, "agent_is_at_queue_trip_destination", "A001", journey)
    state["positions"]["A001"]["point"] = {"x": 1.5, "y": 2.5502}
    assert not _answer(world, "agent_is_at_queue_trip_destination", "A001", journey)
    state["positions"]["A001"]["point"] = {"x": 1.5, "y": 2.5499}
    assert _answer(world, "agent_is_at_queue_trip_destination", "A001", journey)
    assert not _answer(world, "agent_is_at_queue_trip_destination", "A001", {})
    assert not _answer(world, "agent_is_at_queue_trip_destination", "A001", None)
    assert not _answer(world, "agent_is_at_queue_trip_destination", "A001", {"route_points": []})


def test_a_journey_in_a_dict_of_another_kind_is_a_kernel_refusal(generated_scenario: Any) -> None:
    """Callers pass the journey of the movement row, a plain dict."""

    class Other(dict):
        pass

    world = WorldState.from_scenario(generated_scenario)
    with pytest.raises(KernelRefusal, match="another kind of dict"):
        movement.agent_is_at_queue_trip_destination(world, "A001", Other())


@pytest.mark.parametrize("boundary", [None, 0, 7])
def test_the_staff_positions_asked_from_outside_the_walk(
    generated_scenario: Any, boundary: int | None
) -> None:
    """Nobody travels at the start: each staff member stands at the centre of their zone."""
    world = WorldState.from_scenario(generated_scenario)
    geometry = movement._geometry(world)
    expected = {}
    for staff_id in sorted(world.staff):
        zone_id = world.staff[staff_id].zone_id
        center = movement._zone_center(geometry, zone_id)
        assert center is not None
        expected[staff_id] = {
            "zone_id": zone_id,
            "point": {"x": center.x, "y": center.y},
            "edge_id": None,
            "progress": 0.0,
            "position_kind": "physical",
        }
    answer = movement.staff_position_snapshots(world, boundary_minute=boundary)
    assert answer == expected
    assert list(answer) == sorted(world.staff)
    # The records are written, and the answer holds copies of them.
    state = world.model_state[KEY]["staff_positions"]
    assert dict(state) == expected
    assert all(answer[key] is not state[key] for key in answer)


def test_without_the_movement_state_or_a_floor_plan() -> None:
    world = _without_a_floor_plan()
    agent_id = sorted(world.agents)[0]
    facility_id = facility_of(world, "toilet")
    if KEY in world.model_state:
        del world.model_state[KEY]
    assert not _answer(world, "agent_is_at_queue_slot", agent_id, facility_id)
    assert _answer(world, "agent_is_at_facility_service_point", agent_id, facility_id)
    # No plan: a route of no edges and no minutes from where the person is, and no points.
    zone_id = world.agents[agent_id].zone_id
    assert movement.facility_approach_route(world, agent_id, facility_id) == (
        ZoneRoute(zone_id, world.facilities[facility_id].zone_id, (zone_id,), (), 0),
        (),
    )

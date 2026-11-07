"""A resource-queue stall record follows the Agent at the head of the queue.

`prepare_facility_queue` drops the record once its Agent no longer heads the queue: after the
queue's new order, and after someone gives up. The orders themselves are checked in
tests/integration/test_queue_rules.py; that a generated Scenario with entered stock then runs a
whole day, in tests/integration/test_entered_stock.py.
"""

from __future__ import annotations

import pytest

from sheltersim.core.world import WorldState
from sheltersim.domain.models import ActiveTask, Activity
from sheltersim.domain.queueing import prepare_facility_queue
from sheltersim.domain.resource_service import SERVICE_STATE_KEY
from sheltersim.domain.simulator_state import initialize_simulator_extensions
from sheltersim.experiments.scenario import ScenarioSpec


def _world(scenario: ScenarioSpec) -> WorldState:
    world = WorldState.from_scenario(scenario)
    initialize_simulator_extensions(world)
    return world


def _meal_station(world: WorldState):
    return next(item for item in world.facilities.values() if item.kind == "meal_distribution")


def _line_up(world: WorldState, facility_id: str, hunger: dict[str, float]) -> None:
    """These people in the meal queue, in this order, with this hunger, since minute 0."""
    queue = world.facilities[facility_id].resource_queue_agent_ids
    for agent_id, need in hunger.items():
        agent = world.agents[agent_id]
        agent.activity = Activity.QUEUED
        agent.target_id = facility_id
        agent.needs.hunger = need
        queue.append(agent_id)
        task_id = f"queue-task-{agent_id}"
        world.active_tasks[task_id] = ActiveTask(
            id=task_id,
            kind="resource_service_queue",
            agent_id=agent_id,
            resource_id=facility_id,
            start_minute=0,
            due_minute=0,
        )


def test_stall_is_kept_while_its_agent_heads_the_queue(generated_scenario) -> None:
    world = _world(generated_scenario)
    facility = _meal_station(world)
    first, second = sorted(world.agents)[:2]
    _line_up(world, facility.id, {first: 0.9, second: 0.1})
    stalls = world.model_state[SERVICE_STATE_KEY]["stall_state"]
    stalls[facility.id] = {"agent_id": first, "reason": "staff"}

    prepare_facility_queue(world, facility.id, resource=True, minute=5)

    assert facility.resource_queue_agent_ids == [first, second]
    assert stalls[facility.id] == {"agent_id": first, "reason": "staff"}


@pytest.mark.parametrize("remaining", ["reordered", "left", "empty"])
def test_stall_is_dropped_once_its_agent_no_longer_heads_the_queue(
    generated_scenario, remaining: str
) -> None:
    # Need-weighted reordering or a stalled head leaving used to leave the stall
    # pointing at another Agent, and the next state validation stopped the Run.
    world = _world(generated_scenario)
    facility = _meal_station(world)
    first, second = sorted(world.agents)[:2]
    if remaining == "empty":
        # The head gives up after a minute, and walks nowhere without a position.
        world.model_configs["facility_queues"] = {
            "schema_version": 1,
            "abandon_after_minutes_by_service": {"meal": 1},
        }
        world.model_state["movement_v1"]["positions"][first]["point"] = None
    _line_up(
        world,
        facility.id,
        {
            "reordered": {first: 0.1, second: 0.9},
            "left": {second: 0.5},
            "empty": {first: 0.1},
        }[remaining],
    )
    state = world.model_state[SERVICE_STATE_KEY]
    state["stall_state"][facility.id] = {"agent_id": first, "reason": "staff"}
    history = list(state["queue_stall_history"])

    prepare_facility_queue(world, facility.id, resource=True, minute=5)

    assert (
        facility.resource_queue_agent_ids
        == {
            "reordered": [second, first],
            "left": [second],
            "empty": [],
        }[remaining]
    )
    assert facility.id not in state["stall_state"]
    assert state["queue_stall_history"] == history

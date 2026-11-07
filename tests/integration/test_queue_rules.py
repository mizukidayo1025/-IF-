"""The kernels of native/src/rules/queues.rs, rules of domain/queueing.py and domain/movement.py.

They have no Python rule left (`kernel_only`). The cases check what the rule returned and wrote,
values derived from the deleted Python rules: each coefficient at the two sides of where it changes
the result, and each branch the kernel refuses. For `progress_physical_queue_positions` each test's
results and whole worlds after each call, in canonical JSON (which keeps a float's bits), are
chained into one digest and compared with the deleted Python rule's (ANSWERS, at the end).
"""

from __future__ import annotations

import copy
import hashlib
import math
from collections.abc import Iterator
from typing import Any

import pytest

from sheltersim import native
from sheltersim.contracts.extensions import Point2D
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain import movement, queueing
from sheltersim.domain.models import ActiveTask, Activity
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink

QUEUE_POLICY = "need-weighted-physical-queue-v1"


def _kernel(name: str) -> Any:
    return getattr(native.rules("queues"), name)


def _run(scenario: Any, minutes: int) -> WorldState:
    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
    return world


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _answer(world: WorldState, result: Any) -> bytes:
    """What the rule returned and left, as the deleted rule's answers were recorded."""
    return repr(
        (
            repr(result),
            _bytes(world),
            sorted(world.id_counters.items()),
            [repr(event) for event in world.event_queue.pending()],
        )
    ).encode()


# Each test's answers, chained in the order of its calls of `_same_after`.
_CHAINS: dict[str, Any] = {}
_NODE = [""]


@pytest.fixture(autouse=True)
def _answers_are_the_deleted_rule_s(request: pytest.FixtureRequest) -> Iterator[None]:
    """After each test, its chained answers against the deleted Python rule's (ANSWERS).

    The worlds hold floats (the floor plan's lengths and points), recorded on Linux; they are the
    same bits on every OS, as the gate's hashes are (lane L13).
    """
    _NODE[0] = request.node.name
    yield
    chain = _CHAINS.pop(request.node.name, None)
    if chain is not None:
        assert chain.hexdigest() == ANSWERS[request.node.name]


def _same_after(world: WorldState, name: str, *arguments: Any, **options: Any) -> Any:
    """The rule on the world, its answer chained for the test's check, the row still the tree's."""
    result = getattr(movement, name)(world, *arguments, **options)
    _CHAINS.setdefault(_NODE[0], hashlib.sha256()).update(_answer(world, result) + b"\n")
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])
    return result


def _prepared(world: WorldState, facility_id: str, **options: Any) -> None:
    """`prepare_facility_queue`, with the movement row still its tree's."""
    assert queueing.prepare_facility_queue(world, facility_id, **options) is None
    _row_is_tree(world)


def _to_service(world: WorldState, facility: str, *, changes: bool, **options: Any) -> bool:
    """The head's walk to service, which writes only when it schedules one."""
    before = _bytes(world)
    result = movement.progress_physical_queue_head_to_service(world, facility, **options)
    assert (_bytes(world) != before) is changes
    _row_is_tree(world)
    return result


def _row_is_tree(world: WorldState) -> None:
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])


def _facility(world: WorldState, kind: str) -> str:
    return next(
        item.id
        for item in sorted(world.facilities.values(), key=lambda item: item.id)
        if kind in item.kind and item.id in movement._geometry(world).facility_points
    )


def _people(world: WorldState, count: int) -> list[str]:
    """Agents with nothing under way, so what a test gives them is all they have."""
    out = []
    for agent_id in sorted(world.agents):
        if agent_id in world.model_state[KEY]["journeys"]:
            continue
        if any(task.agent_id == agent_id for task in world.active_tasks.values()):
            continue
        if any(
            agent_id in item
            for facility in world.facilities.values()
            for item in (
                facility.queue_agent_ids,
                facility.active_agent_ids,
                facility.resource_queue_agent_ids,
                facility.resource_active_agent_ids,
            )
        ):
            continue
        out.append(agent_id)
    return out[:count]


def _line_up(
    world: WorldState,
    facility_id: str,
    people: list[str],
    *,
    resource: bool = False,
    starts: list[int] | None = None,
) -> None:
    """These people in this facility's queue, in the zone's middle, as `request` leaves them."""
    facility = world.facilities[facility_id]
    queue = facility.resource_queue_agent_ids if resource else facility.queue_agent_ids
    center = movement._zone_center(movement._geometry(world), facility.zone_id)
    for place, agent_id in enumerate(people):
        agent = world.agents[agent_id]
        agent.zone_id = facility.zone_id
        agent.activity = Activity.QUEUED
        agent.target_id = facility_id
        queue.append(agent_id)
        task_id = f"queue-task-{agent_id}"
        world.active_tasks[task_id] = ActiveTask(
            id=task_id,
            kind="resource_service_queue" if resource else "toilet_queue",
            agent_id=agent_id,
            resource_id=facility_id,
            start_minute=starts[place] if starts else 0,
            due_minute=starts[place] if starts else 0,
        )
        world.model_state[KEY]["positions"][agent_id].update(
            zone_id=facility.zone_id,
            point={"x": center.x, "y": center.y},
            edge_id=None,
            progress=0.0,
            position_kind="physical",
        )


def _world(scenario: Any) -> WorldState:
    return _run(scenario, 2)


# ---- prepare_facility_queue ----------------------------------------------------------------------


def _near_tie(generated_scenario: Any, waited: int, gap: float) -> list[str]:
    """Two people whose priorities part by `gap`: the first has waited, the second needs more."""
    world = _world(generated_scenario)
    world.model_configs["facility_queues"] = {"persistence_urgency_threshold": 0.0}
    toilet = _facility(world, "toilet")
    first, second = _people(world, 2)
    _line_up(world, toilet, [first, second], starts=[0, waited])
    base = 0.25
    aging = 0.1 * min(1.0, waited / 1440.0)
    world.agents[first].needs.toilet_urgency = base
    world.agents[second].needs.toilet_urgency = base + aging + gap
    _prepared(world, toilet, resource=False, minute=waited)
    return [
        "first" if item == first else "second" for item in world.facilities[toilet].queue_agent_ids
    ]


@pytest.mark.parametrize("waited", [100, 720, 1439, 1440, 3000])
def test_aging_decides_a_near_tie_at_its_exact_weight(generated_scenario: Any, waited: int) -> None:
    """A 1% change of the weight 0.1 or the day 1440 turns one of these two around."""
    assert _near_tie(generated_scenario, waited, 1e-6) == ["second", "first"]
    assert _near_tie(generated_scenario, waited, -1e-6) == ["first", "second"]


def test_a_tie_goes_to_the_earlier_start_then_the_earlier_place(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    world.model_configs["facility_queues"]["abandon_after_minutes_by_service"] = {"toilet": 10080}
    toilet = _facility(world, "toilet")
    people = _people(world, 3)
    # Needs at the top of the range, so waits make no difference past a day.
    _line_up(world, toilet, people, starts=[5, 5, 0])
    for agent_id in people:
        world.agents[agent_id].needs.toilet_urgency = 1.0
    _prepared(world, toilet, resource=False, minute=10000)
    assert world.facilities[toilet].queue_agent_ids == [people[2], people[0], people[1]]


@pytest.mark.parametrize(
    ("kind", "field"),
    [("water_station", "hydration_need"), ("meal_distribution", "hunger")],
)
def test_each_service_reads_its_own_need(generated_scenario: Any, kind: str, field: str) -> None:
    world = _world(generated_scenario)
    facility = _facility(world, kind)
    first, second = _people(world, 2)
    _line_up(world, facility, [first, second], resource=True)
    # Everything else says the other way round.
    for name in ("toilet_urgency", "hydration_need", "hunger"):
        setattr(world.agents[first].needs, name, 0.7)
        setattr(world.agents[second].needs, name, 0.2)
    setattr(world.agents[first].needs, field, 0.2)
    setattr(world.agents[second].needs, field, 0.7)
    _prepared(world, facility, resource=True, minute=5)
    assert world.facilities[facility].resource_queue_agent_ids == [second, first]


def test_a_need_past_the_range_counts_as_its_end(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    first, second = _people(world, 2)
    _line_up(world, toilet, [first, second], starts=[0, 3])
    world.agents[first].needs.toilet_urgency = 1.0
    world.agents[second].needs.toilet_urgency = 1.5
    _prepared(world, toilet, resource=False, minute=3)
    assert world.facilities[toilet].queue_agent_ids == [first, second]


def test_someone_with_no_queue_task_leaves_the_list(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    first, second = _people(world, 2)
    _line_up(world, toilet, [first, second])
    del world.active_tasks[f"queue-task-{first}"]
    _prepared(world, toilet, resource=False, minute=3)
    assert world.facilities[toilet].queue_agent_ids == [second]


def test_the_list_is_left_alone_while_someone_walks_in_it(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    first, second = _people(world, 2)
    _line_up(world, toilet, [first, second])
    world.agents[first].needs.toilet_urgency = 0.1
    world.agents[second].needs.toilet_urgency = 0.9
    world.minute_index = 3
    assert _same_after(world, "progress_physical_queue_positions", toilet, resource=False, minute=3)
    assert first in world.model_state[KEY]["journeys"]
    before = _bytes(world)
    _prepared(world, toilet, resource=False, minute=3)
    assert world.facilities[toilet].queue_agent_ids == [first, second]
    assert _bytes(world) == before


def _nobody_left(world: WorldState, facility: str, people: list[str], *, resource: bool) -> None:
    queue = world.facilities[facility]
    assert (queue.resource_queue_agent_ids if resource else queue.queue_agent_ids) == people
    assert _people_who_left(world) == []
    assert "facility_queue_abandon_count" not in world.metric_state
    assert all(world.agents[agent_id].activity is Activity.QUEUED for agent_id in people)


@pytest.mark.parametrize(
    ("kind", "limit", "field"),
    [
        ("toilet", 120, "toilet_urgency"),
        ("water_station", 240, "hydration_need"),
        ("meal_distribution", 360, "hunger"),
    ],
)
def test_each_default_limit_and_the_threshold_are_where_someone_gives_up(
    generated_scenario: Any, kind: str, limit: int, field: str
) -> None:
    resource = kind != "toilet"

    def lined_up(need: float) -> tuple[WorldState, str, str]:
        world = _world(generated_scenario)
        world.duration_minutes = 2000
        world.model_configs["facility_queues"] = {"schema_version": 1}
        facility = _facility(world, kind)
        person = _people(world, 1)[0]
        _line_up(world, facility, [person], resource=resource)
        setattr(world.agents[person].needs, field, need)
        return world, facility, person

    below = math.nextafter(0.8, 0.0)
    world, facility, person = lined_up(below)
    _prepared(world, facility, resource=resource, minute=limit - 1)
    _nobody_left(world, facility, [person], resource=resource)
    world, facility, person = lined_up(below)
    _prepared(world, facility, resource=resource, minute=limit)
    assert world.metric_state["facility_queue_abandon_count"] == 1.0
    assert world.agents[person].next_decision_at == limit + 15
    assert _people_who_left(world) == [person]
    assert f"queue-task-{person}" not in world.active_tasks
    world, facility, person = lined_up(0.8)
    _prepared(world, facility, resource=resource, minute=limit)
    _nobody_left(world, facility, [person], resource=resource)


def test_a_config_s_limit_and_threshold_are_read(generated_scenario: Any) -> None:
    def lined_up(need: float, threshold: Any = 0.5) -> tuple[WorldState, str, str]:
        world = _world(generated_scenario)
        world.model_configs["facility_queues"] = {
            "schema_version": 1,
            "abandon_after_minutes_by_service": {"toilet": 50},
            "retry_delay_minutes": 3,
            "persistence_urgency_threshold": threshold,
        }
        toilet = _facility(world, "toilet")
        person = _people(world, 1)[0]
        _line_up(world, toilet, [person])
        world.agents[person].needs.toilet_urgency = need
        return world, toilet, person

    world, toilet, person = lined_up(0.4)
    _prepared(world, toilet, resource=False, minute=49)
    _nobody_left(world, toilet, [person], resource=False)
    world, toilet, person = lined_up(0.4)
    _prepared(world, toilet, resource=False, minute=50)
    assert world.metric_state["facility_queue_abandon_count"] == 1.0
    assert _people_who_left(world) == [person]
    assert world.agents[person].next_decision_at == 53
    world, toilet, person = lined_up(0.5)
    _prepared(world, toilet, resource=False, minute=50)
    _nobody_left(world, toilet, [person], resource=False)
    world, toilet, person = lined_up(0.4, 1)
    _prepared(world, toilet, resource=False, minute=50)
    assert _people_who_left(world) == [person]
    world, toilet, person = lined_up(0.4, 0)
    _prepared(world, toilet, resource=False, minute=50)
    _nobody_left(world, toilet, [person], resource=False)


@pytest.mark.parametrize("schema", [1.0, True])
def test_a_schema_version_python_counts_as_1_is_read(generated_scenario: Any, schema: Any) -> None:
    """`config.get("schema_version", 1) != 1` let these through, and so does the kernel."""
    world = _waited_long(_world(generated_scenario), schema_version=schema)
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    _prepared(world, toilet, resource=False, minute=60)
    assert _people_who_left(world) == [person]


def _people_who_left(world: WorldState) -> list[str]:
    return [
        record.event.payload["agent_id"]
        for record in world.event_log.records
        if record.event.event_type == "facility_queue_abandon"
    ]


def _waited_long(world: WorldState, need: float = 0.1, **config: Any) -> WorldState:
    world.model_configs["facility_queues"] = {
        "schema_version": 1,
        "abandon_after_minutes_by_service": {"toilet": 50, "water": 50, "meal": 50},
        **config,
    }
    for agent in world.agents.values():
        agent.needs.toilet_urgency = need
        agent.needs.hydration_need = need
        agent.needs.hunger = need
    return world


def _away_from_the_middle(world: WorldState, agent_id: str, distance: float = 0.3) -> None:
    """Someone in their zone but not at its middle, who walks back when they leave."""
    agent = world.agents[agent_id]
    center = movement._zone_center(movement._geometry(world), agent.zone_id)
    world.model_state[KEY]["positions"][agent_id]["point"] = {
        "x": center.x + distance,
        "y": center.y,
    }


def test_someone_who_gives_up_walks_back(generated_scenario: Any) -> None:
    world = _waited_long(_world(generated_scenario), retry_delay_minutes=7)
    toilet = _facility(world, "toilet")
    first, second, third = _people(world, 3)
    _line_up(world, toilet, [first, second, third])
    _away_from_the_middle(world, second)
    _away_from_the_middle(world, third, 0.6)
    sequence = world.id_counters.get("event", 0)
    _prepared(world, toilet, resource=False, minute=60)
    assert world.facilities[toilet].queue_agent_ids == []
    assert _people_who_left(world) == [first, second, third]
    assert world.agents[first].activity is Activity.IDLE
    assert world.agents[first].target_id is None
    assert world.agents[second].activity is Activity.MOVING
    assert world.agents[second].target_id == world.agents[second].zone_id
    assert world.agents[second].next_decision_at == 67
    assert world.model_state[KEY]["journeys"][second]["purpose"] == "queue_abandon"
    assert world.metric_state["facility_queue_abandon_count"] == 3.0
    assert not [task for task in world.active_tasks if task.startswith("queue-task-")]
    # The first takes one sequence; each who walks takes one more for the walk.
    left = [
        record
        for record in world.event_log.records
        if record.event.event_type == "facility_queue_abandon"
    ]
    assert [record.event.event_id for record in left] == [
        f"facility-queue-abandon-{sequence}",
        f"facility-queue-abandon-{sequence + 1}",
        f"facility-queue-abandon-{sequence + 3}",
    ]
    record = left[0]
    assert record.detail == (
        "Agent left a prolonged low-urgency queue and will reconsider after a bounded delay"
    )
    assert (record.realized_minute, record.event.due_minute, record.event.priority) == (60, 60, 0)
    assert record.event.payload == {
        "facility_id": toilet,
        "agent_id": first,
        "task_id": f"queue-task-{first}",
        "service": "toilet",
        "start_minute": 0,
        "wait_minutes": 60,
        "retry_at_minute": 67,
        "reason": "need_weighted_wait_threshold",
    }
    assert record.event.source_id == "ASM-NEED-WEIGHTED-PHYSICAL-QUEUE-V1"
    assert record.event.causation_id == f"queue-task-{first}"
    assert record.event.provenance == {
        "source_type": "assumption",
        "source_id": "ASM-NEED-WEIGHTED-PHYSICAL-QUEUE-V1",
        "policy_version": "need-weighted-physical-queue-v1",
        "transition": "abandon_queue_then_retry",
    }
    walks = {
        event.payload["agent_id"]: event
        for event in world.event_queue.pending()
        if event.event_id.startswith("queue-abandon-arrival-")
    }
    assert sorted(walks) == sorted([second, third])
    walk = walks[second]
    assert walk.event_id == f"queue-abandon-arrival-{sequence + 2}"
    assert walk.event_type == "agent_travel_complete"
    assert walk.causation_id == f"facility-queue-abandon-{sequence + 1}"
    assert walk.payload["travel_minutes"] > 0
    assert walk.due_minute == 60 + walk.payload["travel_minutes"]
    assert walk.payload["purpose"] == "queue_abandon"
    assert walk.payload["task_id"] == f"queue-abandon-travel-{sequence + 2}"
    zone = world.agents[second].zone_id
    assert walk.payload["route_zone_ids"] == (zone, zone)
    assert walk.payload["route_edge_ids"] == ()
    assert walk.provenance["movement"] == "in_room_return_to_zone_anchor"
    travel = world.active_tasks[walk.payload["task_id"]]
    assert (travel.kind, travel.agent_id, travel.resource_id) == ("travel_return", second, None)
    assert (travel.start_minute, travel.due_minute) == (60, walk.due_minute)


def test_only_those_past_their_limit_give_up_and_the_rest_keep_their_order(
    generated_scenario: Any,
) -> None:
    world = _waited_long(_world(generated_scenario))
    toilet = _facility(world, "toilet")
    first, second, third = _people(world, 3)
    _line_up(world, toilet, [first, second, third], starts=[5, 30, 5])
    world.agents[first].needs.toilet_urgency = 0.9
    _prepared(world, toilet, resource=False, minute=60)
    assert world.facilities[toilet].queue_agent_ids == [first, second]
    assert _people_who_left(world) == [third]


def test_no_way_out_leaves_someone_in_line(generated_scenario: Any, monkeypatch: Any) -> None:
    world = _waited_long(_world(generated_scenario))
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    # Far outside the zone's polygon, so no path starts here.
    _away_from_the_middle(world, person, 10_000.0)
    _prepared(world, toilet, resource=False, minute=60)
    assert world.facilities[toilet].queue_agent_ids == [person]
    assert _people_who_left(world) == []


def test_a_walk_past_the_end_of_the_run_leaves_someone_in_line(generated_scenario: Any) -> None:
    world = _waited_long(_world(generated_scenario))
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    _away_from_the_middle(world, person)
    world.duration_minutes = 61
    _prepared(world, toilet, resource=False, minute=60)
    _nobody_left(world, toilet, [person], resource=False)


def test_giving_up_with_no_position_or_no_movement_state(generated_scenario: Any) -> None:
    world = _waited_long(_world(generated_scenario))
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    world.model_state[KEY]["positions"][person]["point"] = None
    _prepared(world, toilet, resource=False, minute=60)
    assert _people_who_left(world) == [person]
    assert world.agents[person].activity is Activity.IDLE


def test_giving_up_drops_a_stall_that_is_no_longer_the_head_s(generated_scenario: Any) -> None:
    world = _waited_long(_world(generated_scenario))
    world.duration_minutes = 2000
    water = _facility(world, "water_station")
    first, second = _people(world, 2)
    _line_up(world, water, [first, second], resource=True, starts=[10, 40])
    world.agents[second].needs.hydration_need = 0.9
    world.model_state["resource_service_v1"]["stall_state"][water] = {
        "agent_id": first,
        "reason": "staff",
    }
    _prepared(world, water, resource=True, minute=60)
    assert _people_who_left(world) == [first]
    assert world.model_state["resource_service_v1"]["stall_state"] == {}


def test_giving_up_takes_its_retry_delay_from_the_config(generated_scenario: Any) -> None:
    """A 1% change of the default 15 shows here: the Run's configs all write theirs."""
    world = _waited_long(_world(generated_scenario))
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    world.model_configs["facility_queues"].pop("retry_delay_minutes", None)
    _prepared(world, toilet, resource=False, minute=60)
    assert world.agents[person].next_decision_at == 75


def test_the_reference_fifo_keeps_the_order_of_starts_and_nobody_gives_up(
    generated_scenario_with: Any,
) -> None:
    from sheltersim.policies.queueing import create_queue_policy

    world = _world(generated_scenario_with(queue_policy_id="strict-fifo-queue-v1"))
    assert create_queue_policy(world.model_configs).model_id == "strict-fifo-queue-v1"
    toilet = _facility(world, "toilet")
    first, second = _people(world, 2)
    _line_up(world, toilet, [first, second], starts=[4, 1])
    world.agents[first].needs.toilet_urgency = 0.99
    _prepared(world, toilet, resource=False, minute=9000)
    assert world.facilities[toilet].queue_agent_ids == [second, first]
    assert _people_who_left(world) == []


LIMITS_TEXT = "queue abandonment limits must be within 1..10080 minutes"
SERVICE_TEXT = "queue abandonment limits contain an unsupported service"
RETRY_TEXT = "queue retry delay must be between 1 and 1440 minutes"
THRESHOLD_TEXT = "queue persistence threshold must be normalized to 0..1"
OBJECT_TEXT = "facility_queues config must be an object"


@pytest.mark.parametrize(
    ("config", "text"),
    [
        ({"schema_version": 2}, "facility_queues requires schema_version 1"),
        ({"schema_version": 1, "other": 1}, OBJECT_TEXT),
        ({"abandon_after_minutes_by_service": {"toilet": 0}}, LIMITS_TEXT),
        ({"abandon_after_minutes_by_service": {"toilet": 10081}}, LIMITS_TEXT),
        ({"abandon_after_minutes_by_service": {"toilet": 12.0}}, LIMITS_TEXT),
        ({"abandon_after_minutes_by_service": {"shower": 5}}, SERVICE_TEXT),
        ({"retry_delay_minutes": 0}, RETRY_TEXT),
        ({"retry_delay_minutes": 1441}, RETRY_TEXT),
        ({"persistence_urgency_threshold": 1.5}, THRESHOLD_TEXT),
        ({"persistence_urgency_threshold": True}, THRESHOLD_TEXT),
        ({"persistence_urgency_threshold": float("nan")}, THRESHOLD_TEXT),
        ({"persistence_urgency_threshold": "high"}, THRESHOLD_TEXT),
        ({"persistence_urgency_threshold": 2}, THRESHOLD_TEXT),
        ({"abandon_after_minutes_by_service": []}, SERVICE_TEXT),
        ({"retry_delay_minutes": 3.0}, RETRY_TEXT),
        (
            {
                "schema_version": 1,
                "abandon_after_minutes_by_service": {"meal": 0},
                "retry_delay_minutes": 0,
            },
            LIMITS_TEXT,
        ),
        # The selection of the models reads the configs first, and turns these down.
        ([], "model configs must map names to configuration objects"),
    ],
)
def test_a_config_python_turns_down_raises_the_python_rule_s_error(
    generated_scenario: Any, config: Any, text: str
) -> None:
    """The kernel raises the policy's ValueError, class and text, before changing anything
    (docs/rust-port/lanes/l8-texts-rest.md)."""
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    _line_up(world, toilet, _people(world, 1))
    # The bytes of the world, without a config that JSON can't hold (NaN).
    before = _bytes(world)
    signed = copy.deepcopy(world.model_configs["facility_queues"])
    world.model_configs["facility_queues"] = config
    with pytest.raises(ValueError) as raised:
        queueing.prepare_facility_queue(world, toilet, resource=False, minute=3)
    assert (type(raised.value), str(raised.value)) == (ValueError, text)
    world.model_configs["facility_queues"] = signed
    assert _bytes(world) == before


def _with_stall(world: WorldState, facility: str, agent_id: str) -> None:
    world.model_state["resource_service_v1"]["stall_state"][facility] = {
        "agent_id": agent_id,
        "reason": "stock",
        "minute": 1,
    }


def test_a_stall_stays_with_the_head_and_goes_when_the_head_changes(
    generated_scenario: Any,
) -> None:
    world = _world(generated_scenario)
    facility = _facility(world, "water_station")
    first, second = _people(world, 2)
    _line_up(world, facility, [first, second], resource=True)
    world.agents[first].needs.hydration_need = 0.9
    world.agents[second].needs.hydration_need = 0.1
    _with_stall(world, facility, first)
    history = list(world.model_state["resource_service_v1"]["queue_stall_history"])
    _prepared(world, facility, resource=True, minute=5)
    assert facility in world.model_state["resource_service_v1"]["stall_state"]
    world.agents[first].needs.hydration_need = 0.1
    world.agents[second].needs.hydration_need = 0.9
    _prepared(world, facility, resource=True, minute=5)
    assert world.facilities[facility].resource_queue_agent_ids == [second, first]
    assert facility not in world.model_state["resource_service_v1"]["stall_state"]
    assert world.model_state["resource_service_v1"]["queue_stall_history"] == history


def test_a_stall_goes_when_its_person_is_no_longer_in_the_queue(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    facility = _facility(world, "meal_distribution")
    first, second = _people(world, 2)
    _line_up(world, facility, [second], resource=True)
    _with_stall(world, facility, first)
    _prepared(world, facility, resource=True, minute=5)
    assert facility not in world.model_state["resource_service_v1"]["stall_state"]


def test_a_person_listed_twice_a_strange_kind_and_a_missing_facility(
    generated_scenario: Any,
) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    first = _people(world, 1)[0]
    _line_up(world, toilet, [first])
    world.facilities[toilet].queue_agent_ids.append(first)
    before = _bytes(world)
    # The state's check turns this down; the kernel takes no part of it.
    with pytest.raises(KernelRefusal, match="twice"):
        queueing.prepare_facility_queue(world, toilet, resource=False, minute=3)
    assert _bytes(world) == before
    world.facilities[toilet].queue_agent_ids.pop()
    # `str.casefold`, which no ASCII lowering gives: "İ" folds to "i̇", so this is a toilet, and
    # "TOİLET" folds to "toi̇let", which is not.
    world.facilities[toilet].kind = "İtoilet"
    _prepared(world, toilet, resource=False, minute=3)
    assert world.facilities[toilet].queue_agent_ids == [first]
    for kind in ("TOİLET", "shelf"):
        world.facilities[toilet].kind = kind
        before = _bytes(world)
        with pytest.raises(ValueError) as raised:
            queueing.prepare_facility_queue(world, toilet, resource=False, minute=3)
        assert (type(raised.value), str(raised.value)) == (
            ValueError,
            f"unsupported queue service facility kind {kind!r}",
        )
        assert _bytes(world) == before
    world.facilities[toilet].kind = "toilet"
    before = _bytes(world)
    _prepared(world, "nowhere", resource=False, minute=3)
    assert _bytes(world) == before


# ---- progress_physical_queue_positions and _head_to_service --------------------------------------


def test_the_first_who_is_not_in_their_place_walks_there(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    people = _people(world, 3)
    _line_up(world, toilet, people)
    minute = world.minute_index
    # The head walks to the slot, which holds service back.
    moved = _same_after(
        world, "progress_physical_queue_positions", toilet, resource=False, minute=minute
    )
    assert moved is True
    journey = world.model_state[KEY]["journeys"][people[0]]
    assert journey["purpose"] == "toilet_queue_position"
    event = next(
        item for item in world.event_queue.pending() if item.event_id == journey["event_id"]
    )
    assert event.provenance["fifo_position"] == 0
    assert event.payload["task_id"] == f"queue-task-{people[0]}"
    # While that walk lasts nobody else starts one, and service waits for it.
    assert (
        _same_after(
            world, "progress_physical_queue_positions", toilet, resource=False, minute=minute
        )
        is True
    )
    assert _to_service(world, toilet, changes=False, resource=False, minute=minute) is True


def test_someone_who_is_moved_from_another_activity_is_queued_by_the_walk(
    generated_scenario: Any,
) -> None:
    world = _world(generated_scenario)
    facility = _facility(world, "water_station")
    person = _people(world, 1)[0]
    _line_up(world, facility, [person], resource=True)
    world.agents[person].activity = Activity.IDLE
    world.agents[person].target_id = None
    minute = world.minute_index
    assert _same_after(
        world, "progress_physical_queue_positions", facility, resource=True, minute=minute
    )
    agent = world.agents[person]
    assert agent.activity is Activity.QUEUED and agent.target_id == facility
    assert world.model_state[KEY]["journeys"][person]["purpose"] == "resource_queue_position"


@pytest.mark.parametrize(("away", "walks"), [(0.0499, False), (0.0501, True)])
def test_a_follower_within_the_tolerance_stands_in_their_slot(
    generated_scenario: Any, away: float, walks: bool
) -> None:
    """The head is at the facility, the follower 0.05 m from the slot, either side of it."""
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    head, follower = _people(world, 2)
    _line_up(world, toilet, [head, follower])
    positions = world.model_state[KEY]["positions"]
    geometry = movement._geometry(world)
    route = movement.facility_approach_route(world, head, toilet)
    assert route is not None
    last = route[1][-1]
    positions[head]["point"] = {"x": last.x, "y": last.y}
    slot = movement._queue_slot_position(world, geometry, follower, toilet)
    assert slot is not None
    positions[follower]["point"] = {"x": slot.x + away, "y": slot.y}
    # Standing somewhere else changes the first free slot's route, not the slot: check it holds.
    near = movement._queue_slot_position(world, geometry, follower, toilet)
    assert near == slot
    minute = world.minute_index
    assert not _same_after(
        world, "progress_physical_queue_positions", toilet, resource=False, minute=minute
    )
    assert (follower in world.model_state[KEY]["journeys"]) is walks


def _metric(world: WorldState) -> float:
    return world.metric_state.get("physical_queue_route_blocked_count", 0.0)


def test_a_head_with_no_route_holds_the_queue_and_a_follower_only_those_behind(
    generated_scenario: Any,
) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    first, second = _people(world, 2)
    _line_up(world, toilet, [first, second])
    # In another room there's no route in this one to any slot.
    other_zone = next(
        zone for zone in sorted(world.zones) if zone != world.facilities[toilet].zone_id
    )
    world.agents[first].zone_id = other_zone
    minute = world.minute_index
    moved = _to_service(world, toilet, changes=True, resource=False, minute=minute)
    assert moved is True
    assert _metric(world) == 1.0
    assert first not in world.model_state[KEY]["journeys"]


def _slot_path(world: WorldState, agent_id: str, facility_id: str) -> tuple[Any, ...]:
    """The deleted rule's walk in the room to the person's slot, from where they stand."""
    geometry = movement._geometry(world)
    slot = movement._queue_slot_position(world, geometry, agent_id, facility_id)
    raw = world.model_state[KEY]["positions"][agent_id]["point"]
    path = movement.plan_in_room_route(
        world,
        zone_id=world.facilities[facility_id].zone_id,
        start=Point2D.model_validate(raw),
        destination=slot,
        mobility=world.agents[agent_id].profile.mobility.value,
    )
    assert path is not None
    return path


@pytest.mark.parametrize("phase", ["position", "service"])
def test_a_walk_s_minutes_turn_over_where_python_s_do(generated_scenario: Any, phase: str) -> None:
    """A speed that puts the walk just under 5 minutes: 1% more length would make it 6."""
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    agent = world.agents[person]
    if phase == "position":
        path = _slot_path(world, person, toilet)
        name = "progress_physical_queue_positions"
    else:
        path = movement.facility_approach_route(world, person, toilet)[1]
        name = "progress_physical_queue_head_to_service"
    length = math.fsum(movement._distance(a, b) for a, b in zip(path, path[1:]))
    config = world.model_configs["movement_model"]
    density = movement._max_route_density(world, (agent.zone_id,), movement._geometry(world))
    crowding = min(
        config["maximum_density_slowdown"],
        1.0 + config["density_slowdown_per_person_m2"] * max(0.0, density - 0.5),
    )
    speeds = config["speed_m_per_minute_by_mobility"]
    speeds[agent.profile.mobility.value] = length * crowding / 4.99
    if phase == "position":
        assert _same_after(world, name, toilet, resource=False, minute=world.minute_index) is True
    else:
        options = {"resource": False, "minute": world.minute_index}
        assert _to_service(world, toilet, changes=True, **options) is True
    journey = world.model_state[KEY]["journeys"][person]
    assert journey["due_minute"] - journey["started_minute"] == 5


def test_a_walk_past_the_end_of_the_run_is_counted_and_not_started(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    minute = world.duration_minutes - 1
    assert _same_after(
        world, "progress_physical_queue_positions", toilet, resource=False, minute=minute
    )
    assert _metric(world) == 1.0
    assert person not in world.model_state[KEY]["journeys"]


def test_the_head_walks_from_its_slot_to_the_service_point(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    minute = world.minute_index
    route, path = movement.facility_approach_route(world, person, toilet)
    assert _to_service(world, toilet, changes=True, resource=False, minute=minute) is True
    journey = world.model_state[KEY]["journeys"][person]
    assert journey["purpose"] == "toilet_queue_service"
    assert journey["route_points"] == [point.model_dump(mode="json") for point in path]
    assert journey["due_minute"] == minute + route.travel_minutes
    event = next(
        item for item in world.event_queue.pending() if item.event_id == journey["event_id"]
    )
    assert event.event_type == "agent_travel_complete"
    assert event.due_minute == journey["due_minute"]
    assert event.payload["purpose"] == "toilet_queue_service"
    assert event.payload["task_id"] == f"queue-task-{person}"
    assert event.provenance["queue_phase"] == "service"
    assert event.provenance["fifo_position"] == 0
    assert world.agents[person].activity is Activity.QUEUED
    # At the point, the walk is over and service may start.
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    route = movement.facility_approach_route(world, person, toilet)
    world.model_state[KEY]["positions"][person]["point"] = {
        "x": route[1][-1].x,
        "y": route[1][-1].y,
    }
    assert _to_service(world, toilet, changes=False, resource=False, minute=minute) is False
    assert (
        _same_after(
            world, "progress_physical_queue_positions", toilet, resource=False, minute=minute
        )
        is False
    )


def test_an_empty_queue_and_a_missing_facility(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    minute = world.minute_index
    name = "progress_physical_queue_positions"
    assert _same_after(world, name, toilet, resource=False, minute=minute) is False
    assert _same_after(world, name, "nowhere", resource=False, minute=minute) is True
    assert _to_service(world, toilet, changes=False, resource=False, minute=minute) is False
    assert _to_service(world, "nowhere", changes=False, resource=False, minute=minute) is True


def test_without_a_task_the_rule_raises_its_own_error(generated_scenario: Any) -> None:
    """The deleted rule's ValueError, which the kernel raises before it writes anything."""
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    del world.active_tasks[f"queue-task-{person}"]
    minute = world.minute_index
    before = _bytes(world)
    with pytest.raises(ValueError) as raised:
        movement.progress_physical_queue_positions(world, toilet, resource=False, minute=minute)
    assert str(raised.value) == "physical queue reservation has no matching active queue task"
    assert _bytes(world) == before


def test_int_coordinates_taint_and_are_a_kernel_refusal(generated_scenario: Any) -> None:
    """An int where the row has a float: only a hand-made state has one, and nothing changes."""
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    person = _people(world, 1)[0]
    _line_up(world, toilet, [person])
    world.model_state[KEY]["positions"][person]["point"] = {"x": 1, "y": 1}
    minute = world.minute_index
    before = _bytes(world)
    with pytest.raises(KernelRefusal):
        movement.progress_physical_queue_positions(world, toilet, resource=False, minute=minute)
    assert _bytes(world) == before


def test_a_tick_of_a_queue_forwards_no_whole_namespace(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    toilet = _facility(world, "toilet")
    _line_up(world, toilet, _people(world, 2))
    mirror = world._core.mirror(KEY)
    before = mirror.stats()
    assert _kernel("progress_physical_queue_positions")(
        world, toilet, resource=False, minute=world.minute_index
    )
    assert mirror.stats()["whole_writes"] == before["whole_writes"]
    assert mirror.stats()["forwards"] == before["forwards"]


def test_each_tick_of_a_run_is_python_s_at_every_facility(generated_scenario: Any) -> None:
    """The walk to service has no Python rule left; the gate's hashes hold it in a run."""
    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    reached = 0
    for tick in range(240):
        engine.step()
        if tick % 20:
            continue
        minute = world.minute_index
        for facility in sorted(world.facilities):
            resource = "toilet" not in world.facilities[facility].kind
            reached += bool(
                world.facilities[facility].resource_queue_agent_ids
                if resource
                else world.facilities[facility].queue_agent_ids
            )
            options = {"resource": resource, "minute": minute}
            _same_after(world, "progress_physical_queue_positions", facility, **options)
            _prepared(world, facility, **options)
    assert reached


# The deleted Python rule's answers of each test, chained in the order of its calls: recorded
# once from the Python rule at 2d8fb46, on Linux.
ANSWERS = {
    "test_a_follower_within_the_tolerance_stands_in_their_slot[0.0499-False]": (
        "f972edadbc0ca2e55e182cef93825b2cf70d27cfcae4b6f63e317287946dcf57"
    ),
    "test_a_follower_within_the_tolerance_stands_in_their_slot[0.0501-True]": (
        "3de0a726d52c6e39c98ef235c66f455caf21646b3a741efba2094e2b75caff04"
    ),
    "test_a_walk_past_the_end_of_the_run_is_counted_and_not_started": (
        "24f43e263bc52703e3c9f459026d8cba8f0d26dc90a404a478b0e3ee46f06500"
    ),
    "test_a_walk_s_minutes_turn_over_where_python_s_do[position]": (
        "af7e67cc53d2a4f7af592b7205a1674839f1e4156fc2c715701055aa16087cdf"
    ),
    "test_an_empty_queue_and_a_missing_facility": (
        "1081e598c3d49c567dbb7adcca026cd9ea4ff239532db8acfd731a83a0ac02a0"
    ),
    "test_each_tick_of_a_run_is_python_s_at_every_facility": (
        "be75b1c0e1fdaf61b30e96ad3b3ec0fbe1b8f74751d64c914476aded32034d05"
    ),
    "test_someone_who_is_moved_from_another_activity_is_queued_by_the_walk": (
        "845e8b40162e1f507d430253d46882c629abfc628e9ac4f5eb8b14f0fc12ab70"
    ),
    "test_the_first_who_is_not_in_their_place_walks_there": (
        "25917c5cddb3a15c7332ab486b6e4a664e3136d8bc1e613fbbeb03d7fb1704f3"
    ),
    "test_the_head_walks_from_its_slot_to_the_service_point": (
        "f18ab419ea389faa3e3ca8f9175c31151cb36f134c8da45c0b0632042c0be45f"
    ),
    "test_the_list_is_left_alone_while_someone_walks_in_it": (
        "5a930920ab0ec29b30458a8c916c244349dce7cc95bfc75215360cbf4e3769a5"
    ),
}

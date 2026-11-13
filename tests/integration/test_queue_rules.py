"""The Python entries of native/src/rules/queues.rs: rules of domain/queueing.py and movement.py.

They have no Python rule left (`kernel_only`). The core's rules are checked without Python in
`native/core/tests/queue_rules.rs`, on the same world (`support/core_fixtures_queues.py`, #225).
Here are the entries: each still reaches the kernel and gives its answer, what the state's checks
in Python turn down, the errors Python raises before the kernel, and the mirror of the movement
state. `test_each_tick_of_a_run_is_python_s_at_every_facility` chains, over a Run, what
`progress_physical_queue_positions` returned and the whole world after each call, in canonical JSON
(which keeps a float's bits), into one digest, compared with the deleted Python rule's (ANSWERS).
"""

from __future__ import annotations

import copy
import hashlib
from collections.abc import Iterator
from typing import Any

import pytest
from support.core_fixtures_queues import facility_of, free_people, line_up, queue_world

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain import movement, queueing
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink


def _kernel(name: str) -> Any:
    return getattr(native.rules("queues"), name)


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
    _row_is_tree(world)
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


# ---- prepare_facility_queue ----------------------------------------------------------------------


def test_a_config_the_model_selection_turns_down_raises_python_s_error(
    generated_scenario: Any,
) -> None:
    """The selection of the models reads the configs first, in Python, and its ValueError is the
    entry's, before changing anything. The texts of the queue configs the kernel turns down are
    checked in the core (`a_config_python_turns_down_raises_the_python_rule_s_error`)."""
    world = queue_world(generated_scenario)
    toilet = facility_of(world, "toilet")
    line_up(world, toilet, free_people(world, 1))
    before = _bytes(world)
    signed = copy.deepcopy(world.model_configs["facility_queues"])
    world.model_configs["facility_queues"] = []
    with pytest.raises(ValueError) as raised:
        queueing.prepare_facility_queue(world, toilet, resource=False, minute=3)
    assert (type(raised.value), str(raised.value)) == (
        ValueError,
        "model configs must map names to configuration objects",
    )
    world.model_configs["facility_queues"] = signed
    assert _bytes(world) == before


def test_a_person_listed_twice_is_turned_down_by_the_state_s_check(
    generated_scenario: Any,
) -> None:
    world = queue_world(generated_scenario)
    toilet = facility_of(world, "toilet")
    first = free_people(world, 1)[0]
    line_up(world, toilet, [first])
    world.facilities[toilet].queue_agent_ids.append(first)
    before = _bytes(world)
    # The state's check turns this down; the kernel takes no part of it.
    with pytest.raises(KernelRefusal, match="twice"):
        queueing.prepare_facility_queue(world, toilet, resource=False, minute=3)
    assert _bytes(world) == before


# ---- progress_physical_queue_positions and _head_to_service --------------------------------------


def test_a_head_with_no_route_holds_the_queue_and_a_follower_only_those_behind(
    generated_scenario: Any,
) -> None:
    """The entry of the head's walk to service (the core's test of the same name has the rest)."""
    world = queue_world(generated_scenario)
    toilet = facility_of(world, "toilet")
    first, second = free_people(world, 2)
    line_up(world, toilet, [first, second])
    # In another room there's no route in this one to any slot.
    other_zone = next(
        zone for zone in sorted(world.zones) if zone != world.facilities[toilet].zone_id
    )
    world.agents[first].zone_id = other_zone
    minute = world.minute_index
    moved = _to_service(world, toilet, changes=True, resource=False, minute=minute)
    assert moved is True
    assert world.metric_state.get("physical_queue_route_blocked_count", 0.0) == 1.0
    assert first not in world.model_state[KEY]["journeys"]


def test_int_coordinates_taint_and_are_a_kernel_refusal(generated_scenario: Any) -> None:
    """An int where the row has a float: only a hand-made state has one, and nothing changes."""
    world = queue_world(generated_scenario)
    toilet = facility_of(world, "toilet")
    person = free_people(world, 1)[0]
    line_up(world, toilet, [person])
    world.model_state[KEY]["positions"][person]["point"] = {"x": 1, "y": 1}
    minute = world.minute_index
    before = _bytes(world)
    with pytest.raises(KernelRefusal):
        movement.progress_physical_queue_positions(world, toilet, resource=False, minute=minute)
    assert _bytes(world) == before


def test_a_tick_of_a_queue_forwards_no_whole_namespace(generated_scenario: Any) -> None:
    world = queue_world(generated_scenario)
    toilet = facility_of(world, "toilet")
    line_up(world, toilet, free_people(world, 2))
    mirror = world._core.mirror(KEY)
    before = mirror.stats()
    assert _kernel("progress_physical_queue_positions")(
        world, toilet, resource=False, minute=world.minute_index
    )
    assert mirror.stats()["whole_writes"] == before["whole_writes"]
    assert mirror.stats()["forwards"] == before["forwards"]


def test_each_tick_of_a_run_is_python_s_at_every_facility(generated_scenario: Any) -> None:
    """The walk to service has no Python rule left; the gate's hashes hold it in a run. The entries
    of the walk to the slots and of the queue's order, on every facility of a Run."""
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
# once from the Python rule at 2d8fb46, on Linux. The other tests' were recorded again from the core
# when they moved to native/core/tests/queue_rules.rs (#225).
ANSWERS = {
    "test_each_tick_of_a_run_is_python_s_at_every_facility": (
        "66b22a89a431abdc07962e318ae418ff4b1555eb015631be14a4a8165d19669a"
    ),
}

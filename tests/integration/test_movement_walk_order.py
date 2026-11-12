"""The random walks of `advance_movement`, the branches that depend on the walk's order.

The gate and the 7-day run never reach these branches (docs/rust-port/lanes/l3-movement.md). The
other tests of them are the core's (`native/core/tests/movement_walk_order.rs`, on worlds that
`support/core_fixtures_movement.py` builds, #225); the random walks stay here, as their inputs are
drawn with Python's `random.Random` (#225, decision 5), and are an entry test of
`advance_movement`. Each walk's answers, the result and the whole world after each call (floats by
their bits, with the event numbers, the suppressed records and the metrics), are chained into one
digest and compared with the deleted Python rule's, recorded once from it at 2d8fb46 (ANSWERS, at
the end).
"""

from __future__ import annotations

import hashlib
import random
from collections.abc import Iterator
from typing import Any

import pytest
from support.core_fixtures_movement import (
    TOILET,
    add_task_and_event,
    move_the_toilet,
    toilet_area,
    trips_of_the_facility_s_own_queue,
)

from sheltersim import native
from sheltersim.core.world import WorldState
from sheltersim.domain import movement
from sheltersim.domain.models import ActiveTask
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _records(world: WorldState) -> list[Any]:
    """Each record, with the event's payload by its sorted keys, as the log's bytes hold it."""
    return [
        (
            record.event.event_id,
            record.event.sequence,
            record.realized_minute,
            record.status,
            record.detail,
            sorted(record.event.payload.items(), key=lambda item: item[0]),
        )
        for record in world.event_log.records
    ]


def _pending(world: WorldState) -> list[str]:
    return [repr(event) for event in world.event_queue.pending()]


def _order_of_maps(world: WorldState) -> list[list[str]]:
    state = world.model_state[KEY]
    return [list(state[name]) for name in ("positions", "journeys", "edge_usage_minutes")]


def _answer(world: WorldState, result: tuple[str, ...]) -> bytes:
    """What the rule returned and left, as the deleted rule's answers were recorded."""
    return repr(
        (
            result,
            _bytes(world),
            sorted(world.id_counters.items()),
            sorted(world.metric_state.items()),
            _pending(world),
            _records(world),
            _order_of_maps(world),
            sorted((key, agent.activity, agent.target_id) for key, agent in world.agents.items()),
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


def _same_after(world: WorldState, minute: int) -> tuple[str, ...]:
    """The rule on the world, its answer chained for the test's check, the row still the tree's."""
    result = movement.advance_movement(world, minute)
    _CHAINS.setdefault(_NODE[0], hashlib.sha256()).update(_answer(world, result) + b"\n")
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])
    return result


# ---- At random -----------------------------------------------------------------------------------


def _random_trips(world: WorldState, rng: random.Random, people: tuple[str, ...]) -> None:
    """Everyone in a queue, each with a trip of a kind drawn at random, some of them stale."""
    state = world.model_state[KEY]
    for key in [event.event_id for event in world.event_queue.pending()]:
        if key.startswith("physical-queue-trip-"):
            world.event_queue.remove(key)
    area = toilet_area(world)
    xs = [point.x for point in area.polygon]
    ys = [point.y for point in area.polygon]

    def somewhere() -> dict[str, float]:
        return {
            "x": rng.uniform(min(xs) + 0.2, max(xs) - 0.2),
            "y": rng.uniform(min(ys) + 0.2, max(ys) - 0.2),
        }

    state["journeys"].clear()
    tasks = {
        task.agent_id: key
        for key, task in world.active_tasks.items()
        if task.kind == "toilet_queue"
    }
    for agent_id in rng.sample(people, len(people)):
        kind = rng.choice(("position", "service", "other", "none", "stale", "gone"))
        state["positions"][agent_id]["point"] = somewhere()
        state["positions"][agent_id]["progress"] = rng.choice((0.0, 0.25, 1.0))
        if kind == "none":
            continue
        event_id = f"trip-{agent_id}"
        purpose = {
            "position": "toilet_queue_position",
            "service": "toilet_queue_service",
            "stale": "toilet_queue_position",
            "gone": "toilet_queue_position",
            "other": "return",
        }[kind]
        task_id = tasks[agent_id]
        if kind == "other":
            task_id = f"other-{agent_id}"
        add_task_and_event(
            world,
            agent_id,
            f"holder-{agent_id}",
            event_id,
            200 + rng.randrange(50),
            "travel_return",
        )
        del world.active_tasks[f"holder-{agent_id}"]
        if kind == "other":
            world.active_tasks[task_id] = ActiveTask(
                id=task_id,
                kind="travel_return",
                agent_id=agent_id,
                resource_id=None,
                start_minute=0,
                due_minute=300,
            )
        if kind == "stale":
            world.event_queue.remove(event_id)
        if kind == "gone":
            task_id = f"task-that-went-{agent_id}"
        state["journeys"][agent_id] = {
            "event_id": event_id,
            "task_id": task_id,
            "purpose": purpose,
            "facility_id": TOILET if kind != "other" else None,
            "destination_zone_id": "toilet",
            "route_zone_ids": ["toilet", "toilet"],
            "route_edge_ids": [],
            "route_points": [somewhere(), somewhere(), somewhere()],
            "started_minute": rng.choice((0, 7)),
            "due_minute": 300,
        }


def _random_walk(scenario: Any, seed: int) -> WorldState:
    rng = random.Random(seed)
    people = tuple(rng.sample(("A001", "A002", "A003", "A004", "A005", "A006"), 6))
    world = trips_of_the_facility_s_own_queue(scenario, people)
    _random_trips(world, rng, people)
    point = world.model_configs["facility_geometry"]["facility_points"][TOILET]
    move_the_toilet(world, {"x": point["x"] - rng.uniform(0.0, 0.4), "y": point["y"]})
    if rng.random() < 0.5:
        area = toilet_area(world)
        xs = [item.x for item in area.polygon]
        ys = [item.y for item in area.polygon]
        x, y = rng.uniform(min(xs), max(xs)), rng.uniform(min(ys), max(ys))
        world.model_configs.setdefault("spatial_constraints", {}).setdefault(
            "obstacles", []
        ).append(
            {
                "id": "random-block",
                "polygon": [
                    {"x": x - 0.2, "y": y - 0.2},
                    {"x": x + 0.2, "y": y - 0.2},
                    {"x": x + 0.2, "y": y + 0.2},
                    {"x": x - 0.2, "y": y + 0.2},
                ],
                "source_ids": ["ASM-TEST"],
            }
        )
    for minute in (1, 2, 3):
        world.minute_index = minute
        _same_after(world, minute)
    return world


@pytest.mark.parametrize("seed", range(40))
def test_random_walks_are_python_s_bit_for_bit(generated_scenario: Any, seed: int) -> None:
    _random_walk(generated_scenario, seed)


def test_the_random_walks_reach_the_branches(generated_scenario: Any) -> None:
    """Or the test above compares little: replans of each ending, and people placed in slots."""
    counts: dict[str, float] = {}
    for seed in range(12):
        world = _random_walk(generated_scenario, seed)
        for name in (
            "physical_queue_replan_count",
            "physical_queue_replan_blocked_count",
            "movement_replan_count",
        ):
            counts[name] = counts.get(name, 0.0) + world.metric_state.get(name, 0.0)
    assert counts["physical_queue_replan_count"] > 0
    assert counts["physical_queue_replan_blocked_count"] > 0


# The deleted Python rule's answers of each test, chained in the order of its calls: recorded
# once from the Python rule at 2d8fb46, on Linux.
ANSWERS = {
    "test_random_walks_are_python_s_bit_for_bit[0]": (
        "e9d4ef400329a888a7114dd91f166d4f35ab039cd8684ca90fd81e4d8b471861"
    ),
    "test_random_walks_are_python_s_bit_for_bit[10]": (
        "41a29120050a35086846e504893b2d93c3a8b870c20a77b87b2103395a9d7d12"
    ),
    "test_random_walks_are_python_s_bit_for_bit[11]": (
        "afba7b48474a5c2c4089f3102d10146839145f854a6cfe769a692c1831e57310"
    ),
    "test_random_walks_are_python_s_bit_for_bit[12]": (
        "5222b0dde2605e22dad2b104456874e37b130154b48d24eb8be0fda7daee3523"
    ),
    "test_random_walks_are_python_s_bit_for_bit[13]": (
        "576fced6fdb71527e0910fcc1aa4f3ebb2ad78e73d706b06dde3692949696e7b"
    ),
    "test_random_walks_are_python_s_bit_for_bit[14]": (
        "af6d3f9fc3b254102ccafdf332ca4f1701d21cd6ef4454f5a98a318dbdcacdbb"
    ),
    "test_random_walks_are_python_s_bit_for_bit[15]": (
        "dafe25035bd94d22292227bba35ac99fc13728b157fd09c53817b9a2bd67bd66"
    ),
    "test_random_walks_are_python_s_bit_for_bit[16]": (
        "448ab96cfcfc2adb3e96d169503671601734ebf03077e4f2eda22f42e7f1e18c"
    ),
    "test_random_walks_are_python_s_bit_for_bit[17]": (
        "40099415f018c7eb115effd54bb919d2f71937c380538a6c73c7dce37a1af89e"
    ),
    "test_random_walks_are_python_s_bit_for_bit[18]": (
        "88b55191ac1ca920159f181aed9834d9267a811d155cea1f0fdab7eb3f43b26f"
    ),
    "test_random_walks_are_python_s_bit_for_bit[19]": (
        "204b2d9d6b2e3dd0c3929aa2a7337baf9e4e29f20dea5f6ca49ea1d9f3f530a6"
    ),
    "test_random_walks_are_python_s_bit_for_bit[1]": (
        "d9cc5928b92ee549cc6e0fae6921ddeb0b5b6613fc7c248cc1738604b3cbd152"
    ),
    "test_random_walks_are_python_s_bit_for_bit[20]": (
        "c0e9fe977196d02a7041f8f5c0c825907b32bbcadccec8c1f4c84ca7546eb579"
    ),
    "test_random_walks_are_python_s_bit_for_bit[21]": (
        "f0b4bb8f44fadcd975b87312e6ff43a9232b63e7d93abdd1ed49644a7e1c615e"
    ),
    "test_random_walks_are_python_s_bit_for_bit[22]": (
        "6df12f936985a435d8833555c28233a969588938e8cc715d3df4ffb61d36ad9f"
    ),
    "test_random_walks_are_python_s_bit_for_bit[23]": (
        "1151817dfde20df79ef213239f43d954ab93bb3a48134567295b41d9cdaf66b4"
    ),
    "test_random_walks_are_python_s_bit_for_bit[24]": (
        "c89c30b48fcc981f5a1ee16365ff8a613c3b007bccb6cd872f7a1f3d6f27f874"
    ),
    "test_random_walks_are_python_s_bit_for_bit[25]": (
        "4150b2a3cf2df1fe845ede054a6fa97047a44455b4499ae0277de9c62c8e19f0"
    ),
    "test_random_walks_are_python_s_bit_for_bit[26]": (
        "db7e90da71b47baccfbf90cedea30c421ba25967bd2f91cc79d6af98722c69a5"
    ),
    "test_random_walks_are_python_s_bit_for_bit[27]": (
        "7b5e4f0f8a4129064c83a3f03ef866a799199719695b61fccaae3660bcfbfde9"
    ),
    "test_random_walks_are_python_s_bit_for_bit[28]": (
        "7402de92d046ae586656b0fae89d5e688f244545f724ba033e25f794557c7563"
    ),
    "test_random_walks_are_python_s_bit_for_bit[29]": (
        "207f03e80db3befcb0aab2897c75ba0d9fdfc14c0cef0b595b42afed8e27f2d6"
    ),
    "test_random_walks_are_python_s_bit_for_bit[2]": (
        "5126596a7f18bce3cd934226ecb5688d4b0b80ed3e1ff4d7bef73572485ce5c6"
    ),
    "test_random_walks_are_python_s_bit_for_bit[30]": (
        "016d7e72a6c825ea562fb9eee65bfbd3047652c9370bfbbe55da0fba14350054"
    ),
    "test_random_walks_are_python_s_bit_for_bit[31]": (
        "1eaea0056d3cf6d609b86e711c3c055bee66c08a4d0bd1693ccea133e201cde9"
    ),
    "test_random_walks_are_python_s_bit_for_bit[32]": (
        "872a83a5ef21504539415ef05fe1542d8bfe10fc9cb8f67893447a2107039e57"
    ),
    "test_random_walks_are_python_s_bit_for_bit[33]": (
        "4facff7c31c5a58b81fa55cad55df077e58119056c685d27f3ce74ad84f82957"
    ),
    "test_random_walks_are_python_s_bit_for_bit[34]": (
        "fcfac9525839b7471b616f9e94b08cc73c13ede02807399bc33e8bd65c959978"
    ),
    "test_random_walks_are_python_s_bit_for_bit[35]": (
        "69bfaa1957d8f79ee55c402902a8db8c020c26a0b12748be5be96cc2b48e564d"
    ),
    "test_random_walks_are_python_s_bit_for_bit[36]": (
        "b5e3a1fb3623d630a25d804bd9d81011aa3597d5293b86f71ea5fc493b40b7ec"
    ),
    "test_random_walks_are_python_s_bit_for_bit[37]": (
        "9c3916e7d636737f0eb42c1d81dce25a4c4d2312754fdae7812fa9043fd20cdc"
    ),
    "test_random_walks_are_python_s_bit_for_bit[38]": (
        "04d6806f8a192a59f1bafc1ef3eae2222b835c96d8b23fab4cdd897f93c9d54c"
    ),
    "test_random_walks_are_python_s_bit_for_bit[39]": (
        "2e2501024a9bab81e0c0bbce98c43b544c5b3e75d5393be7b14957c48bb853bc"
    ),
    "test_random_walks_are_python_s_bit_for_bit[3]": (
        "46a41a0fc0c74b77898cd7eab677df0a50b374876fe09205535b7dc5a2dc6c5b"
    ),
    "test_random_walks_are_python_s_bit_for_bit[4]": (
        "cedcc8a93dc4127d3647b7ad39304c4ff1fe67455acf40ba1d1b7c0424bc6315"
    ),
    "test_random_walks_are_python_s_bit_for_bit[5]": (
        "c555e309667ec6658d4502eaed664254097e769b2c39f97072bdcda97fa47db6"
    ),
    "test_random_walks_are_python_s_bit_for_bit[6]": (
        "f0c69e33592db044fe0304d650a9f938cbf14e776aa36b16303178e9ef64d08e"
    ),
    "test_random_walks_are_python_s_bit_for_bit[7]": (
        "9f23142f43447c157d89fc70e139a49e544333d92917a951c988d3281a7f2567"
    ),
    "test_random_walks_are_python_s_bit_for_bit[8]": (
        "9bf6510a56e69ebee18829dd0bf99cb034987f4f0d005a4e1124b5c22eae385f"
    ),
    "test_random_walks_are_python_s_bit_for_bit[9]": (
        "bc5fda3f7f9842b5de54f599f4da3f0a117287ea5195530176446fdc36774074"
    ),
    "test_the_random_walks_reach_the_branches": (
        "e645a90957f99fdd442d5bffa517e0dee590fdbfe96a60ec741e5e432ac85363"
    ),
}

"""The rows are the truth and the entity objects are their views (Issue #152, PR 6-1).

A Rust rule writes a row only, and puts in or takes out rows by key; when its borrow of the rows
ends, the objects follow before any Python runs (native/src/state/entities.rs, EntityStore.flush):
the fields that differ are written into the object, an object is made from each new row with no
``__init__`` and goes to its key's place in the map, and an object whose row left gets the row's
last values and leaves. These tests drive one table at a time with ``apply_row_ops`` and check what
Python sees: ``is``, ``vars()``, ``==``, enum members, the order of the maps, detached objects,
whole-map assignment, ``_bare_world`` and deepcopy.
"""

from __future__ import annotations

import copy
import pickle
from dataclasses import fields, replace
from types import SimpleNamespace
from typing import Any

import pytest
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.state_mirror import (
    TrackedList,
    check_entity_rows,
    entity_row_mismatches,
    watch_entity_rows,
)
from sheltersim.core.world import WorldState
from sheltersim.domain.models import (
    ActiveTask,
    Activity,
    AgentState,
    FacilityState,
    NeedState,
    StaffState,
    SupportRequestState,
    SupportRequestStatus,
)
from sheltersim.experiments.scenario import ScenarioSpec


def _world() -> WorldState:
    return WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))


def _ops(world: WorldState, field: str, *ops: tuple[Any, ...]) -> None:
    native.state("entities").apply_row_ops(world, field, list(ops))


def _task(task_id: str, **changes: Any) -> SimpleNamespace:
    values = {
        "id": task_id,
        "kind": "travel",
        "agent_id": None,
        "resource_id": None,
        "start_minute": 0,
        "due_minute": 10,
        "staff_id": None,
    }
    values.update(changes)
    return SimpleNamespace(**values)


def _request(request_id: str, agent_id: str, **changes: Any) -> SimpleNamespace:
    values = {
        "id": request_id,
        "agent_id": agent_id,
        "requested_minute": 0,
        "timeout_minute": 30,
        "status": SupportRequestStatus.PENDING,
        "assigned_staff_id": None,
        "task_id": None,
        "resolved_minute": None,
        "cooldown_until": 0,
        "last_assignment_attempt_minute": None,
        "last_assignment_wait_reason_code": None,
    }
    values.update(changes)
    return SimpleNamespace(**values)


def _consistent(world: WorldState) -> None:
    store = world._core.entities()
    assert store.tainted is None
    assert store.verify(world) is None


def _with_tasks(world: WorldState, *keys: str) -> None:
    for key in keys:
        world.active_tasks[key] = ActiveTask(
            id=key, kind="travel", agent_id=None, resource_id=None, start_minute=0, due_minute=5
        )


def test_an_inserted_row_gets_an_object_at_the_end_of_its_map() -> None:
    world = _world()
    _with_tasks(world, "T1", "T2")
    _ops(world, "active_tasks", ("insert", "T3", _task("T3", staff_id="S1", due_minute=12)))
    assert list(world.active_tasks) == ["T1", "T2", "T3"]
    made = world.active_tasks["T3"]
    assert type(made) is ActiveTask
    assert made._store is world._core.entities()
    assert made == ActiveTask("T3", "travel", None, None, 0, 12, "S1")
    assert list(vars(made)) == [entry.name for entry in fields(ActiveTask)]
    assert world.active_tasks["T3"] is made
    _consistent(world)


def test_an_object_made_from_a_row_is_what_its_constructor_makes() -> None:
    world = _world()
    agent_id = next(iter(world.agents))
    _ops(
        world,
        "support_requests",
        ("insert", "R1", _request("R1", agent_id, status=SupportRequestStatus.UNRESOLVED)),
    )
    made = world.support_requests["R1"]
    built = SupportRequestState("R1", agent_id, 0, 30)
    built.status = SupportRequestStatus.UNRESOLVED
    assert made == built
    assert vars(made) == vars(built)
    assert list(vars(made)) == list(vars(built))
    assert made.status is SupportRequestStatus.UNRESOLVED
    staff = next(iter(world.staff.values()))
    copied = SimpleNamespace(**{entry.name: getattr(staff, entry.name) for entry in fields(staff)})
    copied.id = "S-new"
    copied.work_queue = ["T9"]
    _ops(world, "staff", ("insert", "S-new", copied))
    member = world.staff["S-new"]
    assert member == StaffState(
        "S-new", staff.zone_id, staff.skills, ["T9"], staff.busy_until, staff.active_task_id
    )
    assert member.skills == staff.skills and type(member.skills) is tuple
    assert type(member.work_queue) is TrackedList and member.work_queue == ["T9"]
    member.work_queue.append("T10")
    assert world._core.entities().row("staff", "S-new")["work_queue"] == ["T9", "T10"]
    _consistent(world)


def test_a_new_facility_has_tracked_lists_that_reach_its_row() -> None:
    world = _world()
    facility = next(iter(world.facilities.values()))
    copied = SimpleNamespace(
        **{entry.name: getattr(facility, entry.name) for entry in fields(facility)}
    )
    copied.id = "F-new"
    copied.queue_agent_ids = ["A1"]
    _ops(world, "facilities", ("insert", "F-new", copied))
    made = world.facilities["F-new"]
    assert type(made) is FacilityState
    assert made.queue_agent_ids == ["A1"]
    for name in ("active_agent_ids", "queue_agent_ids", "resource_active_agent_ids"):
        assert type(getattr(made, name)) is TrackedList
    made.active_agent_ids.append("A2")
    assert world._core.entities().row("facilities", "F-new")["active_agent_ids"] == ["A2"]
    assert list(world.facilities)[-1] == "F-new"
    _consistent(world)


def test_a_new_agent_has_its_needs_and_its_frozen_profile() -> None:
    world = _world()
    agent = next(iter(world.agents.values()))
    copied = SimpleNamespace(**{entry.name: getattr(agent, entry.name) for entry in fields(agent)})
    copied.profile = replace(agent.profile, id="A-new")
    copied.needs = NeedState(0.1, 0.2, 0.3, 0.4)
    _ops(world, "agents", ("insert", "A-new", copied))
    made = world.agents["A-new"]
    assert type(made) is AgentState and type(made.needs) is NeedState
    assert made.profile == copied.profile
    assert made.profile.initial_needs == agent.profile.initial_needs
    with pytest.raises(AttributeError):
        made.profile.id = "other"
    assert made.needs == NeedState(0.1, 0.2, 0.3, 0.4)
    assert made.needs._store is world._core.entities()
    made.needs.hunger = 0.9
    assert world._core.entities().row("agents", "A-new")["needs"]["hunger"] == 0.9
    assert made == AgentState(
        profile=copied.profile,
        zone_id=agent.zone_id,
        activity=agent.activity,
        target_id=agent.target_id,
        needs=NeedState(0.1, 0.9, 0.3, 0.4),
        stress=agent.stress,
        trust=agent.trust,
        next_decision_at=agent.next_decision_at,
        support_request_id=agent.support_request_id,
        support_cooldown_until=agent.support_cooldown_until,
    )
    _consistent(world)


def test_a_row_written_alone_is_followed_by_its_object_field_by_field() -> None:
    world = _world()
    agent = next(iter(world.agents.values()))
    zone = agent.zone_id
    needs = agent.needs
    _ops(
        world,
        "agents",
        ("write", agent.id, "stress", 0.625),
        ("write", agent.id, "activity", Activity.MOVING),
        (
            "write",
            agent.id,
            "needs",
            NeedState(needs.toilet_urgency, 0.75, needs.fatigue, needs.hydration_need),
        ),
    )
    assert agent.stress == 0.625
    assert agent.activity is Activity.MOVING
    # The needs object stays; only the field that differs is written into it.
    assert agent.needs is needs and needs.hunger == 0.75
    assert agent.zone_id is zone
    assert world.agents[agent.id] is agent
    _consistent(world)


def test_a_removed_row_leaves_its_object_with_the_last_values() -> None:
    world = _world()
    _with_tasks(world, "T1", "T2", "T3")
    old = world.active_tasks["T2"]
    _ops(world, "active_tasks", ("write", "T2", "due_minute", 99), ("remove", "T2"))
    assert list(world.active_tasks) == ["T1", "T3"]
    assert old._store is None
    assert old.due_minute == 99
    # A plain dataclass now: its writes stay with it.
    old.due_minute = 7
    assert old.due_minute == 7
    _consistent(world)


def test_a_key_replaced_by_the_core_keeps_its_place() -> None:
    world = _world()
    _with_tasks(world, "T1", "T2", "T3")
    old = world.active_tasks["T2"]
    _ops(world, "active_tasks", ("insert", "T2", _task("T2", kind="other")))
    assert list(world.active_tasks) == ["T1", "T2", "T3"]
    assert world.active_tasks["T2"] is not old
    assert world.active_tasks["T2"].kind == "other"
    assert old._store is None and old.kind == "travel"
    _consistent(world)


@pytest.mark.parametrize(
    ("ops", "keys"),
    [
        ([("insert", "T9", _task("T9")), ("remove", "T9")], ["T1", "T2"]),
        ([("remove", "T1"), ("insert", "T1", _task("T1"))], ["T2", "T1"]),
        ([("insert", "T1", _task("T1")), ("remove", "T1")], ["T2"]),
        (
            [("insert", "T1", _task("T1")), ("insert", "T1", _task("T1", due_minute=3))],
            ["T1", "T2"],
        ),
        (
            [("remove", "T1"), ("insert", "T9", _task("T9")), ("insert", "T1", _task("T1"))],
            ["T2", "T9", "T1"],
        ),
    ],
)
def test_puts_and_takes_in_one_borrow_leave_the_map_in_the_rows_order(
    ops: list[tuple[Any, ...]], keys: list[str]
) -> None:
    world = _world()
    _with_tasks(world, "T1", "T2")
    _ops(world, "active_tasks", *ops)
    assert list(world.active_tasks) == keys
    assert world._core.entities().keys("active_tasks") == keys
    _consistent(world)


def test_the_objects_follow_a_borrow_that_fails() -> None:
    world = _world()
    _with_tasks(world, "T1")
    with pytest.raises(KeyError):
        _ops(world, "active_tasks", ("write", "T1", "due_minute", 44), ("remove", "missing"))
    assert world.active_tasks["T1"].due_minute == 44
    _consistent(world)


def test_a_whole_map_assigned_takes_the_core_inserts() -> None:
    world = _world()
    _with_tasks(world, "T1")
    kept = world.active_tasks["T1"]
    world.active_tasks = {"T1": kept}
    _ops(world, "active_tasks", ("insert", "T2", _task("T2")))
    assert list(world.active_tasks) == ["T1", "T2"]
    assert world.active_tasks["T1"] is kept
    _consistent(world)


def test_a_deep_copy_keeps_the_order_and_the_values_of_the_views() -> None:
    world = _world()
    _with_tasks(world, "T1", "T2")
    _ops(world, "active_tasks", ("insert", "T0", _task("T0")), ("remove", "T1"))
    copied = copy.deepcopy(world)
    assert list(copied.active_tasks) == ["T2", "T0"]
    assert copied.active_tasks["T0"] == world.active_tasks["T0"]
    assert copied.active_tasks["T0"] is not world.active_tasks["T0"]
    assert copied._canonical_text(copied._canonical_data()) == world._canonical_text(
        world._canonical_data()
    )
    _consistent(world)
    _consistent(copied)


def test_a_made_object_copies_and_pickles_as_a_plain_object() -> None:
    world = _world()
    _ops(world, "active_tasks", ("insert", "T5", _task("T5")))
    made = world.active_tasks["T5"]
    for other in (copy.copy(made), copy.deepcopy(made), pickle.loads(pickle.dumps(made))):
        assert other == made and other._store is None
        assert list(vars(other)) == list(vars(made))


def test_a_bare_world_of_detached_objects_shares_nothing_with_the_rows() -> None:
    world = _world()
    _with_tasks(world, "T1")
    plain = {key: copy.copy(value) for key, value in world.active_tasks.items()}
    bare = WorldState._bare_world(active_tasks=plain)
    assert "_core" not in bare.__dict__
    assert all(item._store is None for item in bare.active_tasks.values())
    _ops(world, "active_tasks", ("write", "T1", "due_minute", 8))
    assert bare.active_tasks["T1"].due_minute == 5
    _consistent(world)


def test_a_write_past_setattr_is_not_seen_by_the_rows_and_verify_says_so() -> None:
    """Writing obj.__dict__[name] of an attached object is no longer supported (design §5)."""
    world = _world()
    agent = next(iter(world.agents.values()))
    agent.__dict__["stress"] = 0.4321
    assert world._core.entities().verify(world) is not None


def test_the_watch_counts_ticks_whose_objects_differ_from_their_rows() -> None:
    world = _world()
    watch_entity_rows(True)
    try:
        TickEngine(world).run(3)
        assert entity_row_mismatches() == {"checked": 3, "mismatches": 0, "first": None}
        next(iter(world.agents.values())).__dict__["trust"] = 0.123
        check_entity_rows(world, "after")
        found = entity_row_mismatches()
        assert found["mismatches"] == 1 and found["first"].startswith("after: agents.")
    finally:
        watch_entity_rows(False)


def test_a_tainted_store_refuses_the_core_and_keeps_its_objects() -> None:
    world = _world()
    _with_tasks(world, "T1")
    world._core.entities().taint_now("a test")
    with pytest.raises(native.Unsupported):
        _ops(world, "active_tasks", ("insert", "T2", _task("T2")))
    assert list(world.active_tasks) == ["T1"]

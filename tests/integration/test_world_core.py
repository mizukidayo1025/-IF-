"""A world's core: the sample namespace owned end to end, and real rules on tracked trees.

These own more namespaces for one test. The sample's typed row is
native/src/state/sample_namespace.rs; the row that holds any JSON can own a real namespace, which
runs its rules, the kernels and the saves on a tree whose every write reaches Rust. The namespaces
owned in production are in test_psychology_row.py.
"""

from __future__ import annotations

import copy
import json
from collections.abc import Iterator
from concurrent.futures import ThreadPoolExecutor
from typing import Any

import pytest
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core import state_mirror
from sheltersim.core.checkpoint_cache import canonical_bytes
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import (
    DetachedNodeError,
    NamespaceTable,
    TrackedDict,
    TrackedList,
    detach,
    register_rust_validator,
    rust_validated,
)
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.psychological_state import PSYCHOLOGICAL_STATE_KEY
from sheltersim.experiments.phases.segments import install_segments
from sheltersim.experiments.scenario import ScenarioSpec

SAMPLE = "sample_v1"


def _sample_state() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "total": 1.0,
        "people": {
            "a": {"level": 0.25, "visits": 1, "note": None},
            "b": {"level": 0.75, "visits": 2, "note": "n"},
        },
        "notes": [{"k": [1, 2.5]}],
    }


@pytest.fixture
def owned(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    monkeypatch.setitem(state_mirror.OWNED_NAMESPACES, SAMPLE, ("sample_namespace", "ROW"))
    yield


@pytest.fixture
def world(owned: None) -> WorldState:
    world = WorldState("s", 1, 100)
    world.model_state[SAMPLE] = _sample_state()
    world.model_state["other_v1"] = {"plain": True}
    return world


def _kernels() -> Any:
    return native.state("sample_namespace")


def _mirror(world: WorldState) -> Any:
    return world._core.mirror(SAMPLE)


def _level(world: WorldState, person: str = "a") -> float:
    return world.model_state[SAMPLE]["people"][person]["level"]


def test_with_nothing_owned_model_state_is_a_plain_dict_and_there_is_a_core(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.setattr(state_mirror, "OWNED_NAMESPACES", {})
    monkeypatch.setattr(state_mirror, "OWNED_FIELDS", {})
    world = WorldState("s", 1, 100)
    assert type(world.model_state) is dict
    assert world._core.owned_names() == []
    world.model_state["x"] = {"a": 1}
    assert type(world.model_state["x"]) is dict
    assert type(copy.deepcopy(world).model_state) is dict


def test_an_owned_namespace_is_a_tree_and_the_others_are_trees_of_value_rows(
    world: WorldState,
) -> None:
    assert type(world.model_state) is NamespaceTable
    assert type(world.model_state[SAMPLE]) is TrackedDict
    assert type(world.model_state[SAMPLE]["notes"]) is TrackedList
    assert _mirror(world).has_row and _mirror(world).verify()
    # A key no lane owns has a row that holds any JSON (Issue #152, lane 5-3).
    assert type(world.model_state["other_v1"]) is TrackedDict
    other = world._core.mirror("other_v1")
    assert other.has_row and other.verify() and other.value_bytes > 0


def test_a_python_write_reaches_the_row(world: WorldState) -> None:
    state = world.model_state[SAMPLE]
    state["people"]["a"]["level"] = 0.5
    state["people"]["a"]["note"] = "x"
    state["people"]["c"] = {"level": 0.1, "visits": 0, "note": None}
    del state["people"]["b"]
    state["notes"][0]["k"].append(3)
    state["total"] = 0.6
    assert json.loads(_kernels().row_json(world)) == state
    assert _mirror(world).verify() and _mirror(world).tainted is None


def test_a_rust_write_reaches_the_tree_without_coming_back(world: WorldState) -> None:
    forwards = _mirror(world).stats()["forwards"]
    _kernels().set_level(world, "a", 0.5)
    state = world.model_state[SAMPLE]
    assert state["people"]["a"]["level"] == 0.5
    assert state["total"] == 1.25
    assert _mirror(world).stats()["forwards"] == forwards
    assert _mirror(world).verify()
    assert type(state["people"]["a"]) is TrackedDict


def test_a_rust_write_bypasses_an_override_of_the_node(world: WorldState) -> None:
    # The sink would see any call of __setitem__.
    class Counting:
        calls = 0

        def __getattr__(self, name: str) -> Any:
            Counting.calls += 1
            raise AssertionError(f"the override reached the sink: {name}")

    world.model_state[SAMPLE]["people"]["a"]._sink, kept = Counting(), _mirror(world)
    try:
        _kernels().set_level(world, "a", 0.5)
    except AssertionError:  # pragma: no cover
        pytest.fail("a rust write ran the override")
    assert Counting.calls == 0
    world.model_state[SAMPLE]["people"]["a"]._sink = kept
    assert _level(world) == 0.5


@pytest.mark.parametrize(
    ("write", "reason"),
    [
        (lambda state: state["people"]["a"].__setitem__("level", 1), "not a float"),
        (lambda state: state["people"]["a"].__setitem__("visits", 1.0), "not an int"),
        (lambda state: state["people"]["a"].__setitem__("visits", True), "not an int"),
        (lambda state: state["people"]["a"].__setitem__("extra", 1), "no field"),
        (lambda state: state["people"].__setitem__("c", {"level": 1.0}), "other fields"),
        (lambda state: state.__setitem__("new", 1), "no field"),
        (lambda state: state.__delitem__("total"), "which fields"),
        (lambda state: state["people"].__setitem__(1, {}), "not a str"),
        (lambda state: state["notes"].append((1, 2)), "plain JSON"),
    ],
)
def test_a_write_that_does_not_fit_taints_the_namespace(
    world: WorldState, write: Any, reason: str
) -> None:
    expected = copy.deepcopy(world.model_state[SAMPLE])
    write(expected)
    write(world.model_state[SAMPLE])
    mirror = _mirror(world)
    assert reason in mirror.tainted
    assert not mirror.has_row
    # The tree keeps what Python wrote, and kernels step aside.
    assert world.model_state[SAMPLE] == expected
    with pytest.raises(native.Unsupported):
        _kernels().set_level(world, "a", 0.5)
    with pytest.raises(native.Unsupported):
        _kernels().row_json(world)


def test_a_tainted_namespace_keeps_taking_writes_and_can_fit_again(world: WorldState) -> None:
    state = world.model_state[SAMPLE]
    state["people"]["a"]["level"] = 1
    state["people"]["a"]["visits"] = 5
    state["notes"].append({"k": 1})
    assert _mirror(world).refit() is False
    state["people"]["a"]["level"] = 1.0
    assert _mirror(world).refit() is True
    assert _mirror(world).verify() and _mirror(world).tainted is None
    state["people"]["a"]["visits"] = 6
    assert _mirror(world).verify()


def test_a_document_that_does_not_fit_starts_tainted_and_is_stored_as_it_is(owned: None) -> None:
    world = WorldState("s", 1, 100)
    world.model_state[SAMPLE] = {"schema_version": "one"}
    assert "not an int" in _mirror(world).tainted or "other fields" in _mirror(world).tainted
    assert world.model_state[SAMPLE] == {"schema_version": "one"}
    world.model_state[SAMPLE] = None
    assert world.model_state[SAMPLE] is None and _mirror(world).tainted is not None
    world.model_state[SAMPLE] = _sample_state()
    assert _mirror(world).tainted is None and _mirror(world).verify()


def test_writing_a_namespace_back_as_itself_costs_nothing(world: WorldState) -> None:
    state = world.model_state[SAMPLE]
    writes = _mirror(world).stats()["whole_writes"]
    world.model_state[SAMPLE] = state
    assert world.model_state[SAMPLE] is state
    assert _mirror(world).stats()["whole_writes"] == writes


def test_a_whole_namespace_write_makes_a_new_tree_and_row(world: WorldState) -> None:
    old = world.model_state[SAMPLE]
    new = _sample_state()
    new["total"] = 9.0
    world.model_state[SAMPLE] = new
    assert world.model_state[SAMPLE] is not new
    assert json.loads(_kernels().row_json(world))["total"] == 9.0
    # The old tree is out of it: a kept name writes to nobody.
    old["total"] = 7.0
    assert json.loads(_kernels().row_json(world))["total"] == 9.0


def test_removing_a_namespace_leaves_no_row(world: WorldState) -> None:
    del world.model_state[SAMPLE]
    assert not _mirror(world).has_row and _mirror(world).tainted is None
    with pytest.raises(native.Unsupported):
        _kernels().row_json(world)
    world.model_state[SAMPLE] = _sample_state()
    assert _mirror(world).has_row
    assert world.model_state.pop(SAMPLE)["total"] == 1.0
    assert not _mirror(world).has_row


def test_detach_keeps_keys_and_row_and_a_read_rebuilds(world: WorldState) -> None:
    expected = copy.deepcopy(world.model_state[SAMPLE])
    assert detach(world, SAMPLE) is True
    mirror = _mirror(world)
    assert mirror.is_detached
    table = world.model_state
    # Keys need no tree.
    assert SAMPLE in table and list(table) == [SAMPLE, "other_v1"] and len(table) == 2
    assert mirror.is_detached and mirror.stats()["rebuilds"] == 0
    # Rust goes on without a tree.
    _kernels().set_level(world, "a", 0.5)
    assert mirror.is_detached
    expected["people"]["a"]["level"] = 0.5
    expected["total"] = 1.25
    assert world.model_state.get(SAMPLE) == expected
    assert not mirror.is_detached and mirror.stats()["rebuilds"] == 1
    assert type(world.model_state) is NamespaceTable
    assert type(world.model_state[SAMPLE]["people"]) is TrackedDict
    world.model_state[SAMPLE]["total"] = 2.0
    assert mirror.verify()


@pytest.mark.parametrize(
    "read",
    [
        lambda table: table[SAMPLE],
        lambda table: table.get(SAMPLE),
        lambda table: dict(table)[SAMPLE],
        lambda table: dict(table.items())[SAMPLE],
        lambda table: next(iter(table.values())),
        lambda table: table.copy()[SAMPLE],
        lambda table: copy.copy(table)[SAMPLE],
        lambda table: copy.deepcopy(table)[SAMPLE],
        lambda table: json.loads(json.dumps(table))[SAMPLE],
        lambda table: table.pop(SAMPLE),
        lambda table: table.setdefault(SAMPLE),
        lambda table: {**table}[SAMPLE],
        lambda table: (table | {})[SAMPLE],
    ],
)
def test_no_read_of_a_detached_namespace_sees_a_placeholder(world: WorldState, read: Any) -> None:
    detach(world, SAMPLE)
    assert read(world.model_state)["total"] == 1.0


def test_comparing_a_detached_table_reads_the_row_and_builds_nothing(world: WorldState) -> None:
    expected = {SAMPLE: _sample_state(), "other_v1": {"plain": True}}
    detach(world, SAMPLE)
    assert world.model_state == expected
    assert expected == world.model_state
    assert not world.model_state != expected
    assert world.model_state != {**expected, "other_v1": {}}
    expected[SAMPLE]["total"] = 2.0
    assert world.model_state != expected and expected != world.model_state
    other = WorldState("s", 1, 100)
    other.model_state[SAMPLE] = _sample_state()
    other.model_state["other_v1"] = {"plain": True}
    assert world.model_state == other.model_state
    detach(other, SAMPLE)
    assert world.model_state == other.model_state
    assert _mirror(world).is_detached and _mirror(world).stats()["rebuilds"] == 0


def test_copies_and_whole_reads_of_a_detached_table_read_the_row(world: WorldState) -> None:
    detach(world, SAMPLE)
    assert copy.deepcopy(world.model_state)[SAMPLE] == _sample_state()
    assert state_mirror.plain_model_state(world.model_state)[SAMPLE] == _sample_state()
    assert state_mirror.read_only_state(world, SAMPLE) == _sample_state()
    assert SAMPLE in repr(world.model_state)
    assert _mirror(world).is_detached and _mirror(world).stats()["rebuilds"] == 0
    # A kernel reading the whole table sees the placeholder and leaves it to Python.
    assert native.canonical_json(world.model_state) is None
    assert _mirror(world).is_detached


def test_a_node_kept_across_a_detach_raises_on_a_write_and_reads_the_past(
    world: WorldState,
) -> None:
    kept = world.model_state[SAMPLE]["people"]["a"]
    detach(world, SAMPLE)
    with pytest.raises(DetachedNodeError):
        kept["level"] = 0.5
    with pytest.raises(DetachedNodeError):
        kept.pop("level")
    _kernels().set_level(world, "a", 0.5)
    assert kept["level"] == 0.25
    assert _level(world) == 0.5


def test_a_tainted_namespace_does_not_detach(world: WorldState) -> None:
    world.model_state[SAMPLE]["total"] = 1
    assert detach(world, SAMPLE) is False
    assert world.model_state[SAMPLE]["total"] == 1


def test_a_callback_can_swap_the_state_under_a_kernel(world: WorldState) -> None:
    other = WorldState("s", 1, 100)
    other.model_state[SAMPLE] = _sample_state()
    other.model_state["other_v1"] = {"plain": True}
    old_core = world._core

    def swap() -> None:
        world.__dict__.clear()
        world.__dict__.update(other.__dict__)
        state_mirror.owned_again(world, old_core)

    _kernels().set_level_after(world, swap, "a", 0.5)
    assert world._core is not old_core
    assert _level(world) == 0.5
    assert json.loads(_kernels().row_json(world))["people"]["a"]["level"] == 0.5


def test_to_dict_and_from_dict_round_trip(world: WorldState) -> None:
    _kernels().set_level(world, "a", 0.5)
    document = world.to_dict()
    assert type(document["model_state"][SAMPLE]) is dict
    restored = WorldState.from_dict(document)
    assert restored._core is not world._core
    assert type(restored.model_state[SAMPLE]) is TrackedDict
    assert restored.same_state(world) and world.same_state(restored)
    assert _kernels().row_json(restored) == _kernels().row_json(world)
    restored.model_state[SAMPLE]["total"] = 8.0
    assert world.model_state[SAMPLE]["total"] != 8.0
    assert not restored.same_state(world)


def test_a_borrowing_from_dict_leaves_the_document_alone(world: WorldState) -> None:
    document = world.to_dict()
    before = json.dumps(document, sort_keys=True)
    restored = WorldState.from_dict(document, _borrow=True, _validate=False)
    assert type(document["model_state"]) is dict
    assert type(document["model_state"][SAMPLE]) is dict
    assert json.dumps(document, sort_keys=True) == before
    assert restored.same_state(world)


def test_copy_and_swap_moves_the_core_with_the_state(world: WorldState) -> None:
    held = world
    candidate = WorldState.from_dict(world.to_dict())
    candidate.model_state[SAMPLE]["total"] = 3.0
    old_core = held._core
    held.__dict__.clear()
    held.__dict__.update(candidate.__dict__)
    state_mirror.owned_again(held, old_core)
    assert held._core is candidate._core
    assert held._core.is_owned_by(held) and not old_core.is_owned_by(held)
    assert json.loads(_kernels().row_json(held))["total"] == 3.0
    held.model_state[SAMPLE]["total"] = 4.0
    assert _mirror(held).verify()


def test_a_deep_copy_is_owned_by_its_own_core(world: WorldState) -> None:
    _kernels().set_level(world, "a", 0.5)
    other = copy.deepcopy(world)
    assert other._core is not world._core
    assert type(other.model_state) is NamespaceTable
    assert type(other.model_state[SAMPLE]) is TrackedDict
    assert other.same_state(world)
    other.model_state[SAMPLE]["total"] = 9.0
    assert world.model_state[SAMPLE]["total"] == 1.25
    assert _mirror(other).verify() and _mirror(world).verify()


def test_assigning_model_state_owns_the_new_document(world: WorldState) -> None:
    old = world.model_state
    world.model_state = {SAMPLE: _sample_state(), "other_v1": {}}
    assert type(world.model_state) is NamespaceTable
    assert world.model_state is not old
    assert type(world.model_state[SAMPLE]) is TrackedDict
    world.model_state[SAMPLE]["total"] = 2.0
    assert json.loads(_kernels().row_json(world))["total"] == 2.0
    # The old table holds plain containers now.
    old[SAMPLE]["total"] = 7.0
    old[SAMPLE] = {}
    assert json.loads(_kernels().row_json(world))["total"] == 2.0
    world.model_state = {"other_v1": {}}
    assert not _mirror(world).has_row


def test_canonical_json_of_tracked_nodes_is_the_json_of_the_plain_state(world: WorldState) -> None:
    state = world.model_state[SAMPLE]
    plain = json.loads(json.dumps(state))
    expected = json.dumps(
        plain, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False
    ).encode()
    assert native.canonical_json(state) == expected
    assert _kernels().row_json(world) == expected
    assert (
        native.canonical_json(world.model_state)
        == json.dumps(
            {SAMPLE: plain, "other_v1": {"plain": True}},
            sort_keys=True,
            separators=(",", ":"),
            ensure_ascii=False,
            allow_nan=False,
        ).encode()
    )


def test_worlds_on_threads_keep_their_own_state(owned: None) -> None:
    def run(number: int) -> float:
        world = WorldState("s", 1, 100)
        world.model_state[SAMPLE] = _sample_state()
        for step in range(2000):
            world.model_state[SAMPLE]["people"]["a"]["visits"] = step + number
            _kernels().set_level(world, "b", step / 4000)
        assert _mirror(world).verify()
        return world.model_state[SAMPLE]["people"]["a"]["visits"]

    with ThreadPoolExecutor(4) as pool:
        assert list(pool.map(run, range(4))) == [1999, 2000, 2001, 2002]


# ---- The registries -------------------------------------------------------------------------


@pytest.fixture
def validator() -> Iterator[Any]:
    yield register_rust_validator
    register_rust_validator(SAMPLE, None)
    register_rust_validator(
        PSYCHOLOGICAL_STATE_KEY, native.state("psychological_distress").validate
    )


def test_a_rust_validator_accepts_or_leaves_it_to_python(world: WorldState, validator: Any) -> None:
    assert not rust_validated(world, SAMPLE)
    validator(SAMPLE, _kernels().validate)
    assert rust_validated(world, SAMPLE)
    world.model_state[SAMPLE]["people"]["a"]["level"] = 2.0
    assert not rust_validated(world, SAMPLE)
    world.model_state[SAMPLE]["people"]["a"]["level"] = 1
    assert _mirror(world).tainted is not None
    # Unsupported from a tainted namespace: the Python rules decide.
    assert not rust_validated(world, SAMPLE)


def _bad_psychology_world(generated_scenario: Any) -> tuple[WorldState, str]:
    world = WorldState.from_scenario(generated_scenario)
    world.model_state[PSYCHOLOGICAL_STATE_KEY]["schema_version"] = 99
    with pytest.raises(StateInvariantError) as raised:
        world.validate(validate_event_history=False)
    return world, str(raised.value)


def test_a_namespace_rust_accepts_skips_its_python_validation(
    generated_scenario: Any, validator: Any
) -> None:
    world, _ = _bad_psychology_world(generated_scenario)
    validator(PSYCHOLOGICAL_STATE_KEY, lambda _world: True)
    world.validate(validate_event_history=False)


def test_a_namespace_rust_turns_down_keeps_the_exact_error(
    generated_scenario: Any, validator: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world, message = _bad_psychology_world(generated_scenario)

    def unsupported(_world: Any) -> bool:
        raise native.Unsupported("tainted")

    validator(PSYCHOLOGICAL_STATE_KEY, unsupported)
    with pytest.raises(StateInvariantError) as raised:
        world.validate(validate_event_history=False)
    assert str(raised.value) == message

    validator(PSYCHOLOGICAL_STATE_KEY, lambda _world: False)
    with pytest.raises(StateInvariantError) as raised:
        world.validate(validate_event_history=False)
    assert str(raised.value) == message


def test_corruption_of_an_owned_namespace_keeps_the_exact_error(
    generated_scenario: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    def corrupt(world: WorldState) -> None:
        agents = world.model_state[PSYCHOLOGICAL_STATE_KEY]["agents"]
        record = agents[next(iter(agents))]
        record[next(iter(record))] = "not a number"

    plain = WorldState.from_scenario(generated_scenario)
    corrupt(plain)
    with pytest.raises(StateInvariantError) as expected:
        plain.validate(validate_event_history=False)

    monkeypatch.setitem(
        state_mirror.OWNED_NAMESPACES,
        PSYCHOLOGICAL_STATE_KEY,
        ("sample_namespace", "ROW"),
    )
    owned = WorldState.from_scenario(generated_scenario)
    mirror = owned._core.mirror(PSYCHOLOGICAL_STATE_KEY)
    # The sample's row can't read this namespace: it is tainted from the start, on purpose.
    assert mirror.tainted is not None
    corrupt(owned)
    with pytest.raises(StateInvariantError) as raised:
        owned.validate(validate_event_history=False)
    assert str(raised.value) == str(expected.value)


def test_a_segment_is_its_kernel_and_turns_unsupported_into_a_kernel_refusal() -> None:
    class Controller:
        def step(self, value: int) -> str:
            """The step's contract."""

    answers: list[Any] = ["rust"]

    def kernel(_controller: Any, value: int) -> str:
        answer = answers[0]
        if answer is None:
            raise native.Unsupported("not this one")
        return f"{answer} {value}"

    install_segments(Controller, {"step": kernel})
    assert Controller().step(1) == "rust 1"
    assert Controller.step.__doc__ == "The step's contract."
    answers[0] = None
    with pytest.raises(KernelRefusal, match="step refused: not this one"):
        Controller().step(2)
    with pytest.raises(ValueError, match="no step"):
        install_segments(Controller, {"nope": kernel})


def test_a_run_ends_with_every_value_row_equal_to_its_tree(
    generated_scenario_with, tmp_path
) -> None:
    """Rules write the keys no lane owns through their trees; the rows follow (#152, lane 5-3)."""
    from sheltersim.experiments.runner import run_scenario

    scenario = generated_scenario_with(
        operational_conditions={
            "initial_resources": {"status": "entered", "water_ml": 300_000, "food_meal_units": 100},
            "scheduled": [
                {
                    "kind": "water_outage",
                    "facility_id": "water_station_main",
                    "start_minute": 60,
                    "end_minute": 240,
                }
            ],
        }
    )
    world = run_scenario(scenario, tmp_path)

    table = world.model_state
    values = {
        name: mirror
        for name, mirror in table._mirrors.items()
        if name not in state_mirror.OWNED_NAMESPACES
    }
    assert "operational_events_v1" in values
    assert table["operational_events_v1"]["water_outages"]
    for name, mirror in values.items():
        assert mirror.holds(table[name]), name
        assert mirror.tainted is None and mirror.has_row and mirror.verify(), name
        assert mirror.row_json() == native.canonical_json(table[name]), name


# ---- The scalars, resources and id_counters in the core (PR 5-2 of #152) ----------------------


def _plain_world() -> WorldState:
    return WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))


def _scalars_of(world: WorldState) -> dict[str, Any]:
    return {
        "scenario_id": world.scenario_id,
        "seed": (str(world.seed), world.seed),
        "duration_minutes": world.duration_minutes,
        "minute_index": world.minute_index,
        "schema_version": world.schema_version,
    }


def test_the_core_holds_the_scalars_as_the_world_sets_them() -> None:
    world = _plain_world()
    assert world._core.scalars() == _scalars_of(world)
    world.minute_index = 17
    world.scenario_id = "renamed"
    world.schema_version = 6
    world.duration_minutes = 600
    assert world._core.scalars() == _scalars_of(world)
    world.seed = 2**80
    assert world._core.scalars()["seed"] == (str(2**80), None)
    world.seed = -3
    assert world._core.scalars()["seed"] == ("-3", -3)


@pytest.mark.parametrize(
    ("name", "value"),
    [
        ("minute_index", True),
        ("minute_index", 1.0),
        ("minute_index", 2**63),
        ("duration_minutes", None),
        ("schema_version", "6"),
        ("seed", False),
        ("seed", 1.5),
        ("scenario_id", 3),
    ],
)
def test_a_scalar_of_another_type_is_kept_by_the_world_and_a_misfit_in_the_core(
    name: str, value: Any
) -> None:
    world = _plain_world()
    setattr(world, name, value)
    assert getattr(world, name) is value
    assert world._core.scalars()[name] is None
    setattr(world, name, getattr(_plain_world(), name))
    assert world._core.scalars() == _scalars_of(world)


def test_copies_of_a_world_hold_its_scalars() -> None:
    world = _plain_world()
    world.minute_index = 9
    for copied in (copy.deepcopy(world), WorldState.from_dict(world.to_dict())):
        assert copied._core is not world._core
        assert copied._core.scalars() == _scalars_of(world)
        copied.minute_index = 10
        assert world._core.scalars()["minute_index"] == 9


def test_resources_and_id_counters_are_trees_of_rows() -> None:
    world = _plain_world()
    world.resources = {"water_ml": 500, "food_meal_units": 3}
    for name in ("resources", "id_counters"):
        mirror = world._core.mirror(name)
        value = getattr(world, name)
        assert type(value) is TrackedDict
        assert mirror.holds(value)
        assert mirror.row_json() == canonical_bytes(value)
    world.resources["water_ml"] -= 1
    world.id_counters["event"] = 2**70
    world.id_counters["resource_task"] = 4
    assert list(world.resources) == ["water_ml", "food_meal_units"]
    assert list(world.id_counters) == ["agent", "event", "resource_task"]
    for name in ("resources", "id_counters"):
        assert world._core.mirror(name).row_json() == canonical_bytes(getattr(world, name))
    assert copy.copy(world.resources) == {"water_ml": 499, "food_meal_units": 3}
    assert type(copy.deepcopy(world.id_counters)) is dict
    world.validate()


def test_a_whole_resources_or_counters_write_makes_a_new_tree() -> None:
    world = _plain_world()
    mirror = world._core.mirror("resources")
    held = world.resources
    new = {"water_ml": 10}
    world.resources = new
    assert world.resources == new and world.resources is not new
    assert mirror.holds(world.resources) and not mirror.holds(held)
    held["water_ml"] = 1
    assert mirror.row_json() == b'{"water_ml":10}'
    world.resources = world.resources
    assert mirror.holds(world.resources)
    world.id_counters = None
    assert world.id_counters is None
    assert world._core.mirror("id_counters").row_json() is None
    with pytest.raises(native.Unsupported):
        native.state("events").allocate_sequence(world)
    world.id_counters = {"event": 5}
    assert native.state("events").allocate_sequence(world) == 5
    assert world.id_counters == {"event": 6}


def test_a_rust_writer_writes_the_counter_row_and_tree() -> None:
    world = _plain_world()
    mirror = world._core.mirror("id_counters")
    tree = world.id_counters
    before = world.id_counters["event"]
    assert native.state("events").allocate_sequence(world) == before
    assert world.id_counters is tree
    assert world.id_counters["event"] == before + 1
    assert mirror.row_json() == canonical_bytes(tree)
    assert mirror.stats()["forwards"] == 0
    assert world.allocate_event_sequence() == before + 1
    assert mirror.row_json() == canonical_bytes(tree)
    assert mirror.stats()["forwards"] == 1


def test_counters_whose_tree_does_not_fit_still_take_a_rust_write() -> None:
    world = _plain_world()
    mirror = world._core.mirror("id_counters")
    world.id_counters["odd"] = (1, 2)
    assert mirror.tainted is not None
    with pytest.raises(native.Unsupported):
        native.state("events").allocate_sequence(world)

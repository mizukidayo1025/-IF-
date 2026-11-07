"""staff_dynamics_v1 owned by Rust, with the rules of domain/staff_dynamics.py.

native/src/rules/staff_dynamics.rs holds the rules (docs/rust-port/lanes/s-staff-dynamics.md). Their
Python bodies are gone (lane L10, `kernel_only`): a run of a rule is checked against what the Python
rule gave, written down from it before it went (the rule's formula, or a digest of its answers on
the same random inputs), and a value the kernels refuse is KernelRefusal. The checks of a config, of
an override and of a saved namespace stay in Python and are the reference for the Rust ones.
"""

from __future__ import annotations

import copy
import hashlib
import json
import math
import random
import sys
from types import SimpleNamespace
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import detach, rust_validated
from sheltersim.core.world import WorldState
from sheltersim.domain.models import StaffState
from sheltersim.domain.staff_dynamics import (
    SHIFT_STAFF_DYNAMICS_POLICY_VERSION,
    SHIFT_STAFF_DYNAMICS_SOURCE_ID,
    STAFF_DYNAMICS_POLICY_VERSION,
    STAFF_DYNAMICS_STATE_KEY,
    add_staff,
    canonical_staff_dynamics_config,
    create_staff_dynamics_state,
    staff_dynamics_is_available,
    staff_dynamics_on_shift,
    update_staff_dynamics,
    validate_staff_dynamics_state,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink

KEY = STAFF_DYNAMICS_STATE_KEY
IDS = ("S01", "S02", "S03")


def _kernels() -> Any:
    return native.rules("staff_dynamics")


def _text(value: Any) -> str:
    """Float bits, 1 against 1.0, and key order all show."""
    return json.dumps(value)


def _world(**raw_config: Any) -> WorldState:
    world = WorldState(
        "s",
        1,
        500,
        staff={staff_id: StaffState(staff_id, "z", ("support",)) for staff_id in IDS},
        model_configs={"staff_dynamics": raw_config},
    )
    world.model_state[KEY] = create_staff_dynamics_state(world)
    return world


def _state(world: WorldState) -> Any:
    return world.model_state[KEY]


def _mirror(world: WorldState) -> Any:
    return world._core.mirror(KEY)


def _digest(texts: list[str]) -> str:
    """A digest of the texts a rule's answers gave, to pin what the Python rule gave."""
    return hashlib.sha256("\n".join(texts).encode("utf-8")).hexdigest()[:16]


def _new_record() -> dict[str, Any]:
    """The record `create_staff_dynamics_state` and `add_staff` give a staff member."""
    return {
        "consecutive_work_minutes": 0,
        "total_work_minutes": 0,
        "total_rest_minutes": 0,
        "fatigue": 0.0,
        "rest_until_minute": 0,
        "last_activity": "off_duty",
    }


# ---- The row ----


@pytest.mark.parametrize(
    "raw",
    [
        {},
        {"max_continuous_work_minutes_by_staff": {"S01": 60}},
        {"shift_windows_by_staff": {"S01": [{"start_minute": 0, "end_minute": 10}]}},
        {
            "policy_version": SHIFT_STAFF_DYNAMICS_POLICY_VERSION,
            "off_shift_fatigue_recovery_per_minute": 0.01,
            "source_id": "é-source",
        },
    ],
)
def test_a_row_writes_the_bytes_of_the_tree(raw: dict[str, Any]) -> None:
    world = _world(**raw)
    assert _mirror(world).row_json() == native.canonical_json(_state(world))
    before = _text(_state(world))
    assert detach(world, KEY)
    assert _text(_state(world)) == before


def test_a_config_key_an_older_schema_lacks_is_absent_from_the_row_and_its_bytes() -> None:
    world = _world()
    assert "max_continuous_work_minutes_by_staff" not in _state(world)["config"]
    assert _mirror(world).has_row
    del _state(world)["config"]["off_shift_fatigue_recovery_per_minute"]
    del _state(world)["config"]["shift_windows_by_staff"]
    assert _mirror(world).has_row
    assert _mirror(world).row_json() == native.canonical_json(_state(world))
    _state(world)["config"]["shift_windows_by_staff"] = {}
    assert _mirror(world).row_json() == native.canonical_json(_state(world))
    assert _mirror(world).verify()


@pytest.mark.parametrize(
    ("write", "reason"),
    [
        (lambda state: state["staff"]["S01"].__setitem__("fatigue", 1), "not a float"),
        (lambda state: state["staff"]["S01"].__setitem__("fatigue", True), "not a float"),
        (lambda state: state["staff"]["S01"].__setitem__("total_work_minutes", 1.0), "not an int"),
        (
            lambda state: state["staff"]["S01"].__setitem__("last_activity", "asleep"),
            "LastActivity",
        ),
        (lambda state: state["staff"]["S01"].__setitem__("last_activity", 1), "not a str"),
        (lambda state: state["staff"]["S01"].__setitem__("other", 1), "no field"),
        (lambda state: state["staff"]["S01"].__delitem__("fatigue"), "which fields"),
        (lambda state: state["config"].__setitem__("rest_minutes", 30.0), "not an int"),
        (
            lambda state: state["config"].__setitem__(
                "off_shift_fatigue_recovery_per_minute", True
            ),
            "not a float or an int",
        ),
        (lambda state: state["config"].__delitem__("rest_minutes"), "which fields"),
        (lambda state: state["config"].__setitem__("other", 1), "no field"),
        (lambda state: state["config"].__setitem__("shift_windows_by_staff", None), "not a dict"),
    ],
)
def test_a_value_the_row_cant_hold_taints_it_and_the_rules_refuse(write: Any, reason: str) -> None:
    world = _world()
    write(_state(world))
    assert reason in _mirror(world).tainted
    with pytest.raises(native.Unsupported):
        _kernels().update(world, 0)
    before = _text(_state(world))
    with pytest.raises(KernelRefusal):
        update_staff_dynamics(world, 0)
    assert _text(_state(world)) == before
    assert not rust_validated(world, KEY)


@pytest.mark.parametrize("recovery", [0, 1])
def test_an_int_recovery_is_held_as_the_int_and_recovers_as_its_float(recovery: int) -> None:
    """`canonical_staff_dynamics_config` takes 0 and 1 and keeps the int; the Python rule took it
    off the fatigue as the float of its value, which the row does too."""
    world = _world(off_shift_fatigue_recovery_per_minute=recovery)
    assert _state(world)["config"]["off_shift_fatigue_recovery_per_minute"] == recovery
    assert type(_state(world)["config"]["off_shift_fatigue_recovery_per_minute"]) is int
    assert _mirror(world).has_row
    assert _mirror(world).row_json() == native.canonical_json(_state(world))
    world.model_state["operational_events_v1"] = {"staff_status": {"S01": {"available": False}}}
    _state(world)["staff"]["S01"]["fatigue"] = 0.75
    update_staff_dynamics(world, 0)
    record = _state(world)["staff"]["S01"]
    # `max(0.0, record["fatigue"] - recovery)`.
    assert record["fatigue"] == max(0.0, 0.75 - recovery)
    assert type(record["fatigue"]) is float
    assert record["last_activity"] == "off_duty"
    assert record["total_rest_minutes"] == 1
    assert _mirror(world).verify()
    assert rust_validated(world, KEY)


def test_forwarded_writes_reach_the_row() -> None:
    world = _world()
    record = _state(world)["staff"]["S01"]
    record["fatigue"] = 0.5
    record["last_activity"] = "working"
    record["rest_until_minute"] = 9
    _state(world)["config"]["rest_minutes"] = 45
    assert _mirror(world).verify()
    assert _mirror(world).row_json() == native.canonical_json(_state(world))


# ---- update ----


def test_a_tick_writes_no_whole_record_and_nothing_is_forwarded() -> None:
    world = _world()
    world.staff["S01"].active_task_id = "t"
    before = dict(_mirror(world).stats())
    for minute in range(20):
        _kernels().update(world, minute)
    after = _mirror(world).stats()
    assert after["whole_writes"] == before["whole_writes"]
    assert after["forwards"] == before["forwards"]
    assert _state(world)["staff"]["S01"]["consecutive_work_minutes"] == 20
    assert _mirror(world).verify()


def test_the_rule_the_callers_use_runs_the_kernel() -> None:
    world = _world()
    update_staff_dynamics(world, 0)
    assert _state(world)["staff"]["S01"]["total_rest_minutes"] == 0
    assert not hasattr(update_staff_dynamics, "__wrapped__")


def _random_world(rng: random.Random) -> WorldState:
    roster = {
        staff_id: [{"start_minute": rng.randrange(0, 60), "end_minute": rng.randrange(60, 200)}]
        for staff_id in IDS
        if rng.random() < 0.5
    }
    raw: dict[str, Any] = {
        "max_continuous_work_minutes": rng.choice((1, 3, 7, 240)),
        "rest_minutes": rng.choice((1, 2, 30, 480)),
        "off_shift_fatigue_recovery_per_minute": rng.choice((0.0, 0.005, 0.1, 1.0, 1 / 3)),
        "shift_windows_by_staff": roster,
    }
    limits = {staff_id: rng.randrange(1, 9) for staff_id in IDS if rng.random() < 0.3}
    if limits:
        raw["max_continuous_work_minutes_by_staff"] = limits
    world = _world(**raw)
    world.model_configs["policy_registry"] = {
        "staff_fatigue": rng.choice(("staff-fatigue-linear-v1", "staff-fatigue-saturating-v1"))
    }
    if rng.random() < 0.5:
        world.model_state["operational_events_v1"] = {
            "staff_status": {
                staff_id: {"available": rng.random() < 0.5}
                for staff_id in IDS
                if rng.random() < 0.7
            }
        }
    for record in _state(world)["staff"].values():
        record["fatigue"] = rng.random()
        record["consecutive_work_minutes"] = rng.randrange(0, 12)
        record["rest_until_minute"] = rng.choice((0, 0, rng.randrange(0, 400)))
    return world


def _update_run(seed: int) -> list[str]:
    """The namespace after each of 30 updates of a random world, as text."""
    rng = random.Random(seed)
    world = _random_world(rng)
    minutes = sorted(rng.randrange(0, 12_000) for _ in range(30))
    texts = []
    for minute in minutes:
        for staff_id in IDS:
            world.staff[staff_id].active_task_id = "t" if rng.random() < 0.4 else None
        update_staff_dynamics(world, minute)
        texts.append(_text(_state(world)))
        assert _mirror(world).verify()
    return texts


# The digests of `_update_run` with the Python rule, taken before it went (lane L10). The fatigue
# of the saturating formula is libm's `exp`, so they are pinned on Linux, as the gate's hashes are.
UPDATE_DIGESTS = {
    0: "eec89a7a17e1ad1f",
    1: "0f784670ff938487",
    2: "c0d429b464a37812",
    3: "463a9147fa747d9e",
    4: "303fc713c0b14fc7",
    5: "0fc54bc9fad202df",
    6: "77117a11a74284e6",
    7: "85d4068377c9dcde",
    8: "40dea20e9e6d7477",
    9: "af311a3fe1dc51f5",
    10: "675c5f9791dec9ac",
    11: "5106039608f3d262",
    12: "5599c4a498120dd9",
    13: "588bef82ab3c19de",
    14: "b289b355c894cd4f",
    15: "14100b62c635a78d",
    16: "16400d4588f9ce5a",
    17: "a712b1f1ebe95ef1",
    18: "cf42c9342b71e104",
    19: "0f13a2ccafa59e68",
    20: "77863d357e49ba78",
    21: "01a6d436e81f845a",
    22: "453cc99226950e5c",
    23: "0c575b6c2e0d0068",
    24: "903db27c64a6d0cc",
    25: "2c80af006a9a3081",
    26: "7b145ad176bd8daa",
    27: "c57a59c8a8f15a8d",
    28: "a29cdb887c0666cc",
    29: "022d27ac1850fe16",
    30: "f242001e3056f4ef",
    31: "e9c29278b8a0a675",
    32: "8cc685b38c281b73",
    33: "8c365486bef77dc4",
    34: "673e093662a1851a",
    35: "38fd495096b7580d",
    36: "62d505f4312dcaa6",
    37: "1ef875a5a57b5b05",
    38: "763c01150c88ff26",
    39: "bb3e7f0ddef2e792",
}


@pytest.mark.parametrize("seed", range(40))
def test_update_writes_what_the_python_rule_wrote(seed: int) -> None:
    texts = _update_run(seed)
    if sys.platform == "linux":
        assert _digest(texts) == UPDATE_DIGESTS[seed]


def test_a_working_minute_is_the_formula_s_fatigue() -> None:
    """The linear formula's fatigue is `min(1.0, consecutive / maximum)`, 240 by default."""
    world = _world()
    world.staff["S01"].active_task_id = "t"
    update_staff_dynamics(world, 0)
    assert _state(world)["staff"]["S01"] == {
        "consecutive_work_minutes": 1,
        "total_work_minutes": 1,
        "total_rest_minutes": 0,
        "fatigue": 1 / 240,
        "rest_until_minute": 0,
        "last_activity": "working",
    }
    assert _state(world)["staff"]["S02"] == {**_new_record(), "total_rest_minutes": 0}


@pytest.mark.parametrize("saturating", [False, True])
def test_a_fatigue_value_has_the_bits_of_the_formula(saturating: bool) -> None:
    """`min(1.0, c / m)`, or `1.0 - exp(-c / m)` with the model's exp (the pinned libm)."""
    exp = native.rules("parity").exp
    rng = random.Random(1)
    for _ in range(20_000):
        consecutive = rng.randrange(-5, 3_000)
        maximum = rng.randrange(1, 1_441)
        fraction = consecutive / maximum
        expected = 1.0 - exp([-fraction])[0] if saturating else min(1.0, fraction)
        actual = _kernels().fatigue_value(saturating, consecutive, maximum)
        assert math.copysign(1.0, actual) == math.copysign(1.0, expected)
        assert actual.hex() == expected.hex()


@pytest.mark.parametrize(
    ("corrupt", "error"),
    [
        (lambda world: _state(world)["staff"].__delitem__("S02"), KeyError),
        (
            lambda world: (
                _state(world)["staff"]["S01"].__setitem__("consecutive_work_minutes", 240),
                _state(world)["config"].pop("rest_minutes"),
            ),
            KeyError,
        ),
        (
            lambda world: world.model_state.__setitem__(
                "operational_events_v1", {"staff_status": {"S01": {}}}
            ),
            KeyError,
        ),
        (lambda world: world.model_state.__setitem__("operational_events_v1", {}), KeyError),
        (lambda world: world.model_configs.__setitem__("policy_registry", 1), AttributeError),
    ],
)
def test_a_bad_value_changes_nothing_and_is_refused(corrupt: Any, error: type[Exception]) -> None:
    """A state no rule writes: the Python rule raised `error` at the record it reached (a KeyError,
    an AttributeError); the kernel refuses before any change."""
    world = _world()
    corrupt(world)
    before = _text(_state(world))
    with pytest.raises(native.Unsupported):
        _kernels().update(world, 0)
    assert _text(_state(world)) == before
    with pytest.raises(KernelRefusal):
        update_staff_dynamics(world, 0)
    assert _text(_state(world)) == before


@pytest.mark.parametrize(
    "prepare",
    [
        lambda world: world.model_configs.__setitem__(
            "policy_registry", {"staff_fatigue": "another-formula"}
        ),
        lambda world: world.model_configs["staff_dynamics"].__setitem__(
            "max_continuous_work_minutes_by_staff", {"S01": 1.5}
        ),
        lambda world: _state(world)["staff"]["S01"].__setitem__(
            "consecutive_work_minutes", 2**63 - 1
        ),
    ],
)
def test_what_the_kernel_does_not_handle_is_unsupported_before_any_change(prepare: Any) -> None:
    world = _world()
    world.staff["S01"].active_task_id = "t"
    prepare(world)
    before = _text(_state(world))
    with pytest.raises(native.Unsupported):
        _kernels().update(world, 0)
    assert _text(_state(world)) == before


@pytest.mark.parametrize("minute", [True, 1.0, 2**70, "1"])
def test_a_minute_of_another_type_is_refused(minute: Any) -> None:
    world = _world()
    for kernel in (_kernels().update, lambda w, m: _kernels().on_shift(w, "S01", m)):
        with pytest.raises(native.Unsupported):
            kernel(world, minute)


def test_a_world_without_the_namespace_answers_as_the_python_rule_does() -> None:
    world = WorldState("s", 1, 100, staff={"S01": StaffState("S01", "z", ("support",))})
    update_staff_dynamics(world, 0)
    add_staff(world, "S02")
    assert staff_dynamics_on_shift(world, "S01", 0) is True
    assert staff_dynamics_is_available(world, "S01", 0) is True
    assert _kernels().on_shift(world, "S01", 0) is True
    assert _kernels().is_available(world, "S01", 0) is True
    assert rust_validated(world, KEY)
    assert KEY not in world.model_state


def test_a_world_of_another_class_without_the_namespace_is_answered() -> None:
    other = SimpleNamespace(model_state={})
    update_staff_dynamics(other, 0)
    assert staff_dynamics_on_shift(other, "S01", 0) is True


# ---- The readers ----


def _reader_answers(seed: int) -> list[str]:
    """The answers of the two readers to 300 random questions on a random world."""
    rng = random.Random(seed)
    world = _random_world(rng)
    if rng.random() < 0.7:
        world.model_state["staff_operations_v1"] = {
            "staff": {
                staff_id: {"phase": rng.choice(("idle", "travel", "work"))}
                for staff_id in IDS
                if rng.random() < 0.7
            }
        }
    answers = []
    for _ in range(300):
        staff_id = rng.choice((*IDS, "unknown"))
        minute = rng.choice((rng.randrange(0, 300), rng.randrange(0, 40_000)))
        for rule in (staff_dynamics_on_shift, staff_dynamics_is_available):
            answer = rule(world, staff_id, minute)
            assert type(answer) is bool
            answers.append(str(answer))
    return answers


# The digests of `_reader_answers` with the Python rules, taken before they went (lane L10).
READER_DIGESTS = {
    0: "be865b6a855b551b",
    1: "5ce1671b6e3cced0",
    2: "a2b27daea81b210d",
    3: "0cb3252518fe4a71",
    4: "d5239bf961fde5af",
    5: "c29f08cbea2f6474",
    6: "96e8d4a585544fcf",
    7: "18a69749a07982ca",
    8: "abfc4c1316a3095b",
    9: "35c84c9c574d69ae",
    10: "51c0b3136652e507",
    11: "25dd85596477fc97",
    12: "69ace248974238f7",
    13: "df767b4fb890ebf7",
    14: "1dd5af005c701d42",
    15: "94e14a5ca03c418e",
    16: "43a6451bd54f2d3b",
    17: "cb607a53e43865c4",
    18: "870b7f3bcc9fcf6f",
    19: "57a3fa7f7a783140",
    20: "ebf91eb61076c9a7",
    21: "6b6218aeb793fdf8",
    22: "7aa93d87035ca0e1",
    23: "79d036b90df5a078",
    24: "0389dc684fc48437",
    25: "3159f67d7cd55ad4",
    26: "27bdbfac1556e848",
    27: "73e37e74e3661bc8",
    28: "b742dbd43bf0ec1e",
    29: "0831bc9fe6e3e442",
    30: "97e7c7e658469c42",
    31: "da66fc2f37f57089",
    32: "217fa39fddd1cbe9",
    33: "51d052de08f1ebec",
    34: "3febcd9a1e3116b7",
    35: "5ab3757cd355ad6d",
    36: "1b33cb153b09974d",
    37: "73d48e3682d5e5f9",
    38: "2077ef40a7055a33",
    39: "32c7c219ea66522f",
}


@pytest.mark.parametrize("seed", range(40))
def test_the_readers_answer_what_the_python_rules_answered(seed: int) -> None:
    answers = _reader_answers(seed)
    assert set(answers) == {"True", "False"}
    assert _digest(answers) == READER_DIGESTS[seed]


@pytest.mark.parametrize(
    ("operations", "available"),
    [
        # `operation is None or operation.get("phase") == "idle"`.
        ({"staff": {"S01": {}}}, False),
        ({"staff": {"S01": {"phase": "idle"}}}, True),
        ({"staff": {"S01": {"phase": "travel"}}}, False),
        ({"staff": {"S01": None}}, True),
        ({"staff": {}}, True),
        ({}, True),
        (None, True),
    ],
)
def test_the_availability_reader_takes_each_shape_of_the_operations_state(
    operations: Any, available: bool
) -> None:
    world = _world()
    world.model_state["staff_operations_v1"] = operations
    assert staff_dynamics_is_available(world, "S01", 0) is available


def test_a_config_without_windows_is_refused_by_the_readers() -> None:
    """A config of the oldest schema has no `shift_windows_by_staff`; the Python rule raised
    KeyError there."""
    world = _world()
    del _state(world)["config"]["shift_windows_by_staff"]
    with pytest.raises(native.Unsupported):
        _kernels().on_shift(world, "S01", 0)
    with pytest.raises(KernelRefusal):
        staff_dynamics_on_shift(world, "S01", 0)


# ---- add_staff, create ----


@pytest.mark.parametrize("staff_id", ["S09", "S01"])
def test_add_staff_writes_a_new_record_and_the_tree_stays_tracked(staff_id: str) -> None:
    world = _world()
    _state(world)["staff"]["S01"]["fatigue"] = 0.5
    expected = json.loads(_text(_state(world)))
    # `state["staff"][staff_id] = {...}`: a known ID keeps its place, a new one goes last.
    expected["staff"][staff_id] = _new_record()
    add_staff(world, staff_id)
    assert _text(_state(world)) == _text(expected)
    assert _mirror(world).verify()
    _state(world)["staff"][staff_id]["fatigue"] = 0.25
    assert _mirror(world).verify()


@pytest.mark.parametrize(
    "raw",
    [
        None,
        {},
        {"max_continuous_work_minutes_by_staff": {"S02": 5}},
        {"shift_windows_by_staff": {"S02": [{"start_minute": 1, "end_minute": 2}]}},
    ],
)
def test_create_state_builds_what_the_python_rule_built(raw: Any) -> None:
    world = WorldState(
        "s",
        1,
        100,
        staff={staff_id: StaffState(staff_id, "z", ()) for staff_id in ("é", "S02", "S01")},
    )
    if raw is not None:
        world.model_configs["staff_dynamics"] = raw
    assert _text(create_staff_dynamics_state(world)) == _text(_python_state(raw))


def _python_state(raw: Any) -> dict[str, Any]:
    """What `create_staff_dynamics_state` built: the config of `canonical_staff_dynamics_config`
    without empty per-staff limits, and a new record for each member in ID order (by code point)."""
    config = canonical_staff_dynamics_config(copy.deepcopy(raw))
    persisted = dict(config)
    if not persisted["max_continuous_work_minutes_by_staff"]:
        persisted.pop("max_continuous_work_minutes_by_staff")
    return {
        "schema_version": 1,
        "policy_version": config["policy_version"],
        "source_id": config["source_id"],
        "config": persisted,
        "staff": {staff_id: _new_record() for staff_id in ("S01", "S02", "é")},
    }


# ---- The validators ----


def _shift(start: Any = 0, end: Any = 10) -> dict[str, Any]:
    return {"start_minute": start, "end_minute": end}


CONFIGS: list[Any] = [
    None,
    {},
    {"policy_version": STAFF_DYNAMICS_POLICY_VERSION},
    {"policy_version": SHIFT_STAFF_DYNAMICS_POLICY_VERSION},
    {"policy_version": SHIFT_STAFF_DYNAMICS_POLICY_VERSION, "source_id": "x"},
    {"policy_version": "other"},
    {"policy_version": 1},
    {"policy_version": None},
    {"schema_version": 1},
    {"schema_version": 1.0},
    {"schema_version": True},
    {"schema_version": 2},
    {"source_id": ""},
    {"source_id": 3},
    {"other": 1},
    {1: 1},
    [],
    "x",
    {"max_continuous_work_minutes": 1},
    {"max_continuous_work_minutes": 1440},
    {"max_continuous_work_minutes": 0},
    {"max_continuous_work_minutes": 1441},
    {"max_continuous_work_minutes": 240.0},
    {"max_continuous_work_minutes": True},
    {"rest_minutes": 480},
    {"rest_minutes": 481},
    {"rest_minutes": "30"},
    {"max_continuous_work_minutes_by_staff": {"S01": 1}},
    {"max_continuous_work_minutes_by_staff": {"S01": 1441}},
    {"max_continuous_work_minutes_by_staff": {"S01": True}},
    {"max_continuous_work_minutes_by_staff": {"": 1}},
    {"max_continuous_work_minutes_by_staff": []},
    {"shift_windows_by_staff": {}},
    {"shift_windows_by_staff": {"S01": []}},
    {"shift_windows_by_staff": {"S01": [_shift()]}},
    {"shift_windows_by_staff": {"S01": [_shift(10, 20), _shift(0, 10)]}},
    {"shift_windows_by_staff": {"S01": [_shift(0, 11), _shift(10, 20)]}},
    {"shift_windows_by_staff": {"S01": [_shift(5, 5)]}},
    {"shift_windows_by_staff": {"S01": [_shift(-1, 5)]}},
    {"shift_windows_by_staff": {"S01": [_shift(0, 10_081)]}},
    {"shift_windows_by_staff": {"S01": [_shift(0.0, 5)]}},
    {"shift_windows_by_staff": {"S01": [_shift(True, 5)]}},
    {"shift_windows_by_staff": {"S01": [{"start_minute": 0}]}},
    {"shift_windows_by_staff": {"S01": [{**_shift(), "x": 1}]}},
    {"shift_windows_by_staff": {"S01": [_shift(i * 2, i * 2 + 1) for i in range(21)]}},
    {"shift_windows_by_staff": {"S01": [_shift(i * 2, i * 2 + 1) for i in range(22)]}},
    {"shift_windows_by_staff": {"": []}},
    {"shift_windows_by_staff": []},
    {"off_shift_fatigue_recovery_per_minute": 0.0},
    {"off_shift_fatigue_recovery_per_minute": -0.0},
    {"off_shift_fatigue_recovery_per_minute": 0.5},
    {"off_shift_fatigue_recovery_per_minute": 1.0},
    {"off_shift_fatigue_recovery_per_minute": 1.5},
    {"off_shift_fatigue_recovery_per_minute": 0},
    {"off_shift_fatigue_recovery_per_minute": 1},
    {"off_shift_fatigue_recovery_per_minute": True},
    {"off_shift_fatigue_recovery_per_minute": float("nan")},
    {"off_shift_fatigue_recovery_per_minute": float("inf")},
    {"policy_version": STAFF_DYNAMICS_POLICY_VERSION, "shift_windows_by_staff": {"S01": []}},
    {"policy_version": STAFF_DYNAMICS_POLICY_VERSION, "shift_windows_by_staff": {}},
    {"policy_version": STAFF_DYNAMICS_POLICY_VERSION, "off_shift_fatigue_recovery_per_minute": 0.1},
    {"policy_version": STAFF_DYNAMICS_POLICY_VERSION, "off_shift_fatigue_recovery_per_minute": 0.0},
]


def _outcome(rule: Any, *arguments: Any, **options: Any) -> tuple[str, str]:
    try:
        return "value", _text(rule(*arguments, **options))
    except ValueError as error:
        return "error", f"{type(error).__name__}: {error}"


# The configs the kernel's canonical config does not take, of types the rows do not hold, and the
# ones whose `in` raises TypeError in Python (an unhashable policy).
REFUSED_CONFIGS = {9, 10}


@pytest.mark.parametrize(
    "index", range(len(CONFIGS)), ids=[f"config{i}" for i in range(len(CONFIGS))]
)
def test_a_new_namespace_has_the_config_of_the_python_check(index: int) -> None:
    """`create_staff_dynamics_state` makes its config in Rust: what
    `canonical_staff_dynamics_config` gives, which a Scenario's check keeps in Python, or its
    ValueError's text, or KernelRefusal for a value of a type the rows do not hold
    (`schema_version: 1.0`, `True`)."""
    raw = CONFIGS[index]
    world = WorldState("s", 1, 100, staff={"S01": StaffState("S01", "z", ())})
    world.model_configs["staff_dynamics"] = copy.deepcopy(raw)
    if index in REFUSED_CONFIGS:
        with pytest.raises(KernelRefusal):
            create_staff_dynamics_state(world)
        return
    expected = _outcome(canonical_staff_dynamics_config, copy.deepcopy(raw))
    try:
        state = create_staff_dynamics_state(world)
    except ValueError as error:
        assert type(error) is ValueError
        assert ("error", f"ValueError: {error}") == expected
        return
    config = canonical_staff_dynamics_config(copy.deepcopy(raw))
    if not config["max_continuous_work_minutes_by_staff"]:
        config.pop("max_continuous_work_minutes_by_staff")
    assert expected[0] == "value"
    assert _text(state["config"]) == _text(config)


def test_a_new_namespace_shares_the_config_s_windows_as_the_python_rule_did() -> None:
    """`{**defaults, **raw}` keeps the config's own objects, and so did the state's config."""
    windows = {"S01": [_shift()]}
    world = WorldState("s", 1, 100, staff={"S01": StaffState("S01", "z", ())})
    world.model_configs["staff_dynamics"] = {"shift_windows_by_staff": windows}
    state = create_staff_dynamics_state(world)
    held = world.model_configs["staff_dynamics"]["shift_windows_by_staff"]
    assert held == windows
    assert state["config"]["shift_windows_by_staff"] is held


def _valid_for_python(world: WorldState) -> bool:
    try:
        validate_staff_dynamics_state(world)
    except ValueError:
        return False
    return True


CORRUPTIONS: list[Any] = [
    lambda state: state.__setitem__("schema_version", 2),
    lambda state: state.__setitem__("policy_version", "other"),
    lambda state: state.__setitem__("source_id", "other"),
    lambda state: state["config"].__setitem__("schema_version", 2),
    lambda state: state["config"].__setitem__("policy_version", "other"),
    lambda state: state["config"].__setitem__("source_id", ""),
    lambda state: state["config"].__setitem__("max_continuous_work_minutes", 0),
    lambda state: state["config"].__setitem__("max_continuous_work_minutes", 1441),
    lambda state: state["config"].__setitem__("rest_minutes", 0),
    lambda state: state["config"].__setitem__("rest_minutes", 481),
    lambda state: state["config"].__setitem__("off_shift_fatigue_recovery_per_minute", 1.5),
    lambda state: state["config"].__setitem__(
        "off_shift_fatigue_recovery_per_minute", float("nan")
    ),
    lambda state: state["config"].__setitem__("shift_windows_by_staff", {"S09": []}),
    lambda state: state["config"].__setitem__("shift_windows_by_staff", {"": []}),
    lambda state: state["config"].__setitem__(
        "shift_windows_by_staff", {"S01": [_shift(0, 11), _shift(10, 20)]}
    ),
    lambda state: state["config"].__setitem__("max_continuous_work_minutes_by_staff", {"S09": 5}),
    lambda state: state["config"].__setitem__("max_continuous_work_minutes_by_staff", {"S01": 0}),
    lambda state: state["config"].__delitem__("shift_windows_by_staff"),
    lambda state: state["config"].__delitem__("off_shift_fatigue_recovery_per_minute"),
    lambda state: state["staff"].__delitem__("S01"),
    lambda state: state["staff"].__setitem__("S09", state["staff"]["S01"].copy()),
    lambda state: state["staff"]["S01"].__setitem__("fatigue", 1.5),
    lambda state: state["staff"]["S01"].__setitem__("fatigue", -0.1),
    lambda state: state["staff"]["S01"].__setitem__("fatigue", float("nan")),
    lambda state: state["staff"]["S01"].__setitem__("consecutive_work_minutes", -1),
    lambda state: state["staff"]["S01"].__setitem__("total_work_minutes", -1),
    lambda state: state["staff"]["S01"].__setitem__("total_rest_minutes", -1),
    lambda state: state["staff"]["S01"].__setitem__("rest_until_minute", -1),
    lambda state: state["staff"]["S01"].__setitem__("last_activity", "asleep"),
]


@pytest.mark.parametrize("corrupt", CORRUPTIONS)
def test_the_validator_never_takes_what_the_python_rule_turns_down(corrupt: Any) -> None:
    world = _world()
    assert rust_validated(world, KEY)
    corrupt(_state(world))
    assert not _valid_for_python(world)
    assert not rust_validated(world, KEY)


@pytest.mark.parametrize(
    "change",
    [
        lambda state: None,
        lambda state: state["config"].__setitem__("max_continuous_work_minutes_by_staff", {}),
        lambda state: state["config"].__setitem__(
            "max_continuous_work_minutes_by_staff", {"S01": 5}
        ),
        lambda state: state["config"].__setitem__(
            "shift_windows_by_staff", {"S01": [_shift()], "S02": []}
        ),
        lambda state: state["staff"]["S01"].__setitem__("fatigue", 1.0),
        lambda state: state["staff"]["S01"].__setitem__("last_activity", "resting"),
    ],
)
def test_the_validator_takes_what_the_python_rule_passes(change: Any) -> None:
    world = _world(policy_version=SHIFT_STAFF_DYNAMICS_POLICY_VERSION)
    change(_state(world))
    assert _valid_for_python(world)
    assert rust_validated(world, KEY)


def test_the_validator_takes_the_config_of_policy_v1_without_its_later_keys() -> None:
    world = _world(policy_version=STAFF_DYNAMICS_POLICY_VERSION)
    config = _state(world)["config"]
    del config["shift_windows_by_staff"]
    del config["off_shift_fatigue_recovery_per_minute"]
    assert _valid_for_python(world)
    assert rust_validated(world, KEY)
    config["max_continuous_work_minutes_by_staff"] = {}
    assert not _valid_for_python(world)
    assert not rust_validated(world, KEY)


def test_a_person_added_to_the_world_alone_fails_the_validators() -> None:
    world = _world()
    world.staff["S09"] = StaffState("S09", "z", ())
    assert not _valid_for_python(world)
    assert not rust_validated(world, KEY)
    add_staff(world, "S09")
    assert _valid_for_python(world)
    assert rust_validated(world, KEY)


# ---- A real run ----


def test_a_run_never_forwards_a_write_of_the_tick_and_the_kernel_runs(
    generated_scenario: Any,
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    engine.step()
    before = dict(_mirror(world).stats())
    worked = 0
    for _ in range(60):
        engine.step()
        assert _mirror(world).verify()
        worked += sum(
            record["last_activity"] == "working" for record in _state(world)["staff"].values()
        )
    after = _mirror(world).stats()
    assert after["whole_writes"] == before["whole_writes"]
    assert after["forwards"] == before["forwards"]
    assert _mirror(world).tainted is None
    assert worked > 0


def test_the_source_of_a_shift_policy_is_its_own() -> None:
    config = canonical_staff_dynamics_config({"shift_windows_by_staff": {}})
    assert config["source_id"] == SHIFT_STAFF_DYNAMICS_SOURCE_ID

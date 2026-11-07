"""The template namespace: psychological_distress_v1 and metric_state owned by Rust.

native/src/core/row.rs writes the rows from their fields, and native/src/rules/psychology.rs is the
only rule of the distress monitor and the long-term distress
(docs/rust-port/lanes/p5-template-namespace.md): the Python rules of domain/psychological_state.py
and domain/distress_monitor.py are gone (lane L10). What they answered, on small worlds, on values
no scenario reaches and on whole ticks, was recorded once as digests (ANSWERS), which the kernel's
answers must equal on Linux. What their checks turned down, the step refuses (KernelRefusal) before
anything changes.
"""

from __future__ import annotations

import copy
import hashlib
import json
import random
import sys
from types import SimpleNamespace
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import NamespaceTable, TrackedDict, detach, rust_validated
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.models import AgentProfile, AgentState, Mobility
from sheltersim.domain.psychological_state import (
    PSYCHOLOGICAL_STATE_KEY,
    PSYCHOLOGICAL_STATE_POLICY_VERSION,
    validate_psychological_state,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink

METRIC = "metric_state"
THRESHOLD = 0.6
RETENTION = 0.9
ACUTE_WEIGHT = 0.08
HIGH_WEIGHT = 0.02

# What the deleted Python rules answered (record_distress_monitor_tick, update_psychological_state
# and next_long_term_distress, and the update phase's Python step, at 45a9bc9), as sha256 of the
# answers' texts, recorded once on Linux.
ANSWERS: dict[str, str] = {
    "int-baseline-0": ("28b87773ac1abba0cdc2f4063e340a29a44d5dc6fe971e8a6f342aa7605f58ca"),
    "int-baseline-1": ("8f29cf90a210f9f6483025883b1498bf477bc74453f2fc0cace41ee1e73a46f9"),
    "long-term": ("ebe04062e2595b4bda4cc150abce6fa872b992bc6ddeaafbf4726864e4e6100c"),
    "long-term-one": ("dcad63db1936d53bd36d64d9c3fb1c910ea7c52e92b038c07399edb21ef69613"),
    "step-0": ("6fdf198dcd1fb13efa2ad7e8ed5c820cddc6f625e537712155314563d65ce59a"),
    "step-1": ("01d9330214853fcb2de323286e6ef33dda17d20fc2f5483abe464d9e482c20d2"),
    "step-1438": ("244e196d97fbf1b30cdb8cce8ec02d75a1093a1a0b2f07a0ae7ff25b7a140043"),
    "step-1439": ("da61e7b244bcc19a6108ebc0e1f46e4a2f42651b4d358dc3726700418ef282aa"),
    "step-2879": ("8b5deb573505976332761cfc1061bd105df7f33397404b0fa0c4d0baa96a0f0a"),
    "ticks": ("b8b2abdff9bf5cd8869cf24ed87f45c335bd6161d273e6684364b4eb9aa6ea15"),
}


def _answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    if sys.platform == "linux":
        assert digest.hexdigest() == ANSWERS[name], name


def _state(*agents: str) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "last_updated_minute": -1,
        "agents": {agent: _record(0.25) for agent in agents},
    }


def _record(baseline: float) -> dict[str, Any]:
    return {
        "baseline_distress": baseline,
        "long_term_distress": baseline,
        "daily_acute_sum": 0.0,
        "daily_high_distress_minutes": 0,
        "daily_observed_minutes": 0,
        "completed_days": 0,
    }


def _world(state: dict[str, Any] | None = None) -> WorldState:
    return WorldState(
        "s",
        1,
        100,
        model_state={PSYCHOLOGICAL_STATE_KEY: state or _state("a", "b")},
    )


def _mirror(world: WorldState, name: str = PSYCHOLOGICAL_STATE_KEY) -> Any:
    return world._core.mirror(name)


def _kernel() -> Any:
    rule = native.rules("psychology").update_long_term

    def run(controller: Any, world: WorldState, view: Any, deltas: Any, minute: Any) -> None:
        # The kernel reads the people's stress from the rows, as the view copied it from them.
        _people_of(world, view)
        rule(controller, world, view, deltas, minute)

    return run


def _people_of(world: WorldState, view: Any) -> None:
    for key, viewed in view.agents.items():
        if key not in world.agents:
            world.agents[key] = AgentState(
                AgentProfile(key, None, "z", Mobility.INDEPENDENT, 0.0, 1.0, key), "z"
            )
        world.agents[key].stress = viewed.stress


def _controller(
    *,
    baselines: dict[str, Any] | None = None,
    threshold: Any = THRESHOLD,
    version: str = PSYCHOLOGICAL_STATE_POLICY_VERSION,
) -> SimpleNamespace:
    parameters = SimpleNamespace(
        chronic_retention=RETENTION,
        chronic_acute_weight=ACUTE_WEIGHT,
        chronic_high_distress_weight=HIGH_WEIGHT,
        high_distress_threshold=threshold,
        baseline_distress_by_agent=baselines if baselines is not None else {},
    )
    return SimpleNamespace(
        psychology=SimpleNamespace(policy_version=version, parameters=parameters)
    )


def _view(previous: dict[str, Any]) -> SimpleNamespace:
    return SimpleNamespace(
        agents={key: SimpleNamespace(stress=stress) for key, stress in previous.items()}
    )


def _deltas(current: dict[str, Any]) -> tuple[Any, ...]:
    return tuple(SimpleNamespace(agent_id=key, stress=stress) for key, stress in current.items())


def _text(value: Any) -> str:
    """Float bits, 1 against 1.0, and key order all show."""
    return json.dumps(value)


def _documents(world: WorldState) -> str:
    return _text([world.metric_state, world.model_state[PSYCHOLOGICAL_STATE_KEY]])


def _run_ticks(scenario: Any, minutes: int, check: Any = None) -> WorldState:
    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
        if check is not None:
            check(world)
    return world


# ---- The macro, on the sample namespace and on the real rows ------------------------------------


def test_a_row_writes_the_bytes_of_the_tree_with_its_keys_sorted() -> None:
    world = _world()
    # The row lists its fields in the tree's order, which is not the sorted one.
    assert list(world.model_state[PSYCHOLOGICAL_STATE_KEY]) == [
        "schema_version",
        "last_updated_minute",
        "agents",
    ]
    assert _mirror(world).row_json() == native.canonical_json(
        world.model_state[PSYCHOLOGICAL_STATE_KEY]
    )
    world.metric_state["é"] = 0.1
    world.metric_state["z"] = 1e-300
    world.metric_state["a"] = 1e22
    assert _mirror(world, METRIC).row_json() == native.canonical_json(world.metric_state)


def test_a_rebuilt_tree_has_the_values_and_the_order_of_the_first() -> None:
    world = _world()
    before = _text(world.model_state[PSYCHOLOGICAL_STATE_KEY])
    assert detach(world, PSYCHOLOGICAL_STATE_KEY)
    assert _text(world.model_state[PSYCHOLOGICAL_STATE_KEY]) == before


@pytest.mark.parametrize(
    # The overflow message is CPython's: "too large" on Linux, "too big" on Windows.
    ("write", "reason"),
    [
        (lambda record: record.__setitem__("daily_acute_sum", 1), "not a float"),
        (lambda record: record.__setitem__("daily_observed_minutes", 1.0), "not an int"),
        (lambda record: record.__setitem__("daily_observed_minutes", True), "not an int"),
        (lambda record: record.__setitem__("daily_observed_minutes", 2**70), "too "),
        (lambda record: record.__setitem__("extra", 1.0), "no field"),
        (lambda record: record.__delitem__("completed_days"), "which fields"),
        (lambda record: record.clear(), "which fields"),
    ],
)
def test_a_value_the_record_cant_hold_taints_the_namespace(write: Any, reason: str) -> None:
    world = _world()
    write(world.model_state[PSYCHOLOGICAL_STATE_KEY]["agents"]["a"])
    assert reason in _mirror(world).tainted
    assert not _mirror(world).has_row


@pytest.mark.parametrize(
    "write",
    [
        lambda state: state.__setitem__("schema_version", 1.0),
        lambda state: state["agents"].__setitem__(1, _record(0.1)),
        lambda state: state["agents"].__setitem__("c", {"baseline_distress": 0.1}),
        lambda state: state.__setitem__("agents", []),
    ],
)
def test_a_write_outside_the_record_taints_the_namespace_too(write: Any) -> None:
    world = _world()
    write(world.model_state[PSYCHOLOGICAL_STATE_KEY])
    assert _mirror(world).tainted is not None


def test_forwarded_writes_reach_both_rows() -> None:
    world = _world()
    state = world.model_state[PSYCHOLOGICAL_STATE_KEY]
    state["agents"]["a"]["daily_acute_sum"] = 0.5
    state["agents"]["a"]["daily_observed_minutes"] = 1
    state["agents"]["c"] = _record(0.5)
    del state["agents"]["b"]
    state["last_updated_minute"] = 7
    world.metric_state["x"] = 1.5
    world.metric_state["y"] = 2.5
    del world.metric_state["x"]
    world.metric_state.setdefault("z", 3.5)
    world.metric_state.update({"w": 4.5})
    for name in (PSYCHOLOGICAL_STATE_KEY, METRIC):
        assert _mirror(world, name).tainted is None
        assert _mirror(world, name).verify()
    assert json.loads(_mirror(world).row_json())["agents"].keys() == {"a", "c"}
    assert json.loads(_mirror(world, METRIC).row_json()) == {"y": 2.5, "z": 3.5, "w": 4.5}
    world.metric_state.clear()
    assert _mirror(world, METRIC).row_json() == b"{}"


def test_an_int_metric_taints_metric_state_and_python_keeps_it() -> None:
    world = _world()
    world.metric_state["count"] = 1
    assert "not a float" in _mirror(world, METRIC).tainted
    assert world.metric_state["count"] == 1
    assert type(world.metric_state["count"]) is int
    # The step then refuses, and changes nothing.
    with pytest.raises(KernelRefusal):
        _step(_controller(), world, _view({"a": 0.1, "b": 0.1}), _deltas({"a": 0.1, "b": 0.1}), 0)
    assert world.model_state[PSYCHOLOGICAL_STATE_KEY]["last_updated_minute"] == -1


# ---- The worlds ----------------------------------------------------------------------------------


def test_a_world_owns_both_and_a_copy_owns_its_own(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    assert type(world.model_state) is NamespaceTable
    assert type(world.metric_state) is TrackedDict
    assert type(world.model_state[PSYCHOLOGICAL_STATE_KEY]) is TrackedDict
    assert _mirror(world).has_row
    assert _mirror(world, METRIC).has_row

    world.metric_state["x"] = 1.0
    other = copy.deepcopy(world)
    other.metric_state["x"] = 2.0
    assert world.metric_state["x"] == 1.0
    assert json.loads(_mirror(other, METRIC).row_json())["x"] == 2.0
    assert json.loads(_mirror(world, METRIC).row_json())["x"] == 1.0

    again = WorldState.from_dict(world.to_dict())
    again.metric_state["x"] = 3.0
    assert world.metric_state["x"] == 1.0
    assert again.same_state(again)
    assert _mirror(again, METRIC).verify()


def test_ticks_write_values_in_place_and_never_the_whole_namespace(generated_scenario: Any) -> None:
    seen: dict[int, tuple[Any, Any, int]] = {}

    def check(world: WorldState) -> None:
        # A copy-and-swap moves the world to a new core and tree; a tick keeps them.
        mirror = _mirror(world)
        first = seen.setdefault(
            id(world._core),
            (
                world._core,
                world.model_state[PSYCHOLOGICAL_STATE_KEY],
                mirror.stats()["whole_writes"],
            ),
        )
        assert world.model_state[PSYCHOLOGICAL_STATE_KEY] is first[1]
        assert mirror.stats()["whole_writes"] == first[2]
        assert mirror.stats()["forwards"] == 0
        for name in (PSYCHOLOGICAL_STATE_KEY, METRIC):
            assert _mirror(world, name).tainted is None
            assert _mirror(world, name).verify()

    world = _run_ticks(generated_scenario, 30, check)
    assert world.model_state[PSYCHOLOGICAL_STATE_KEY]["last_updated_minute"] == 29


def test_ticks_make_the_world_of_the_deleted_rules(generated_scenario: Any) -> None:
    world = _run_ticks(generated_scenario, 40)
    _answers(
        "ticks",
        [_text(world.model_state[PSYCHOLOGICAL_STATE_KEY]["agents"]), world.canonical_json()],
    )


# ---- The rule, against what the deleted Python rule answered -----------------------------------


def _long_term(record: dict[str, Any], retention: float, acute: float, weight: float) -> float:
    return native.rules("psychology").next_long_term_distress_of(
        record["baseline_distress"],
        record["long_term_distress"],
        record["daily_acute_sum"],
        record["daily_high_distress_minutes"],
        record["daily_observed_minutes"],
        retention,
        acute,
        weight,
    )


def test_a_long_term_value_has_the_bits_of_the_deleted_rule() -> None:
    draws = random.Random(20261031)
    values = []
    for _ in range(20_000):
        observed = draws.randrange(1, 1441)
        high = draws.randrange(0, observed + 1)
        record = {
            "baseline_distress": draws.random(),
            "long_term_distress": draws.random(),
            "daily_acute_sum": draws.random() * observed,
            "daily_high_distress_minutes": high,
            "daily_observed_minutes": observed,
        }
        retention, acute, weight = draws.random(), draws.random(), draws.random()
        values.append(_long_term(record, retention, acute, weight).hex())
    _answers("long-term", values)
    # The comparison can tell: a 1% larger weight is another value.
    record = _record(0.25)
    record.update(
        daily_acute_sum=500.0, daily_high_distress_minutes=100, daily_observed_minutes=1000
    )
    same = _long_term(record, 0.9, 0.08, 0.02)
    _answers("long-term-one", [same.hex()])
    assert _long_term(record, 0.9, 0.08 * 1.01, 0.02).hex() != same.hex()


@pytest.mark.parametrize("minute", [0, 1, 1438, 1439, 2879])
def test_the_step_writes_what_the_deleted_rules_wrote(minute: int) -> None:
    draws = random.Random(minute)
    texts = []
    for _ in range(200):
        agents = [f"A{index:03d}" for index in range(draws.randrange(1, 12))]
        state = _state()
        for agent in agents:
            if draws.random() < 0.8:
                observed = draws.randrange(0, 1439)
                state["agents"][agent] = {
                    "baseline_distress": draws.random(),
                    "long_term_distress": draws.random(),
                    "daily_acute_sum": draws.random() * observed,
                    "daily_high_distress_minutes": draws.randrange(0, observed + 1),
                    "daily_observed_minutes": observed,
                    "completed_days": draws.randrange(0, 9),
                }
        # A record of someone without a delta stays, and still closes its day.
        state["agents"]["gone"] = {
            **_record(0.5),
            "daily_acute_sum": 1.5,
            "daily_observed_minutes": 3,
        }
        state["last_updated_minute"] = minute - 1
        metrics: dict[str, float] = {"other": 1.5}
        for agent in agents:
            if draws.random() < 0.5:
                key = f"acute_distress_monitor:{agent}"
                metrics[f"{key}:current_run"] = float(draws.randrange(0, 5))
                metrics[f"{key}:longest_run"] = float(draws.randrange(0, 9))
                metrics[f"{key}:episodes"] = float(draws.randrange(0, 3))
                metrics[f"{key}:recovered_run_total"] = float(draws.randrange(0, 50))
                metrics[f"{key}:recovered_run_count"] = float(draws.randrange(0, 5))
        previous = {agent: draws.random() for agent in agents}
        current = {agent: draws.random() for agent in agents}
        # The boundary itself, and both sides of it.
        current[agents[0]] = THRESHOLD
        baselines = {agent: draws.random() for agent in agents if draws.random() < 0.5}

        world = WorldState(
            "s", 1, 100, metric_state=metrics, model_state={PSYCHOLOGICAL_STATE_KEY: state}
        )
        _kernel()(
            _controller(baselines=baselines), world, _view(previous), _deltas(current), minute
        )
        written = world.model_state[PSYCHOLOGICAL_STATE_KEY]
        texts += [_text(world.metric_state), _text(written)]
        assert _mirror(world).verify()
        assert _mirror(world, METRIC).verify()
        assert _mirror(world).row_json() == native.canonical_json(written)
        # What was added and what was changed are nodes and values of the row, for later writes.
        for agent in agents:
            world.model_state[PSYCHOLOGICAL_STATE_KEY]["agents"][agent]["completed_days"] = 99
        assert _mirror(world).tainted is None
        assert _mirror(world).verify()
    _answers(f"step-{minute}", texts)


@pytest.mark.parametrize("baseline", [0, 1])
def test_an_int_baseline_of_a_scenario_is_written_as_the_deleted_rules_wrote_it(
    baseline: int,
) -> None:
    """`baseline_distress_by_agent` allows ints (StrictInt in the Scenario's psychology config).
    The deleted rules wrote their float(), as the kernel does."""
    state = _state("a", "b")
    state["last_updated_minute"] = 1438
    previous = {"a": 0.1, "b": 0.2, "new": 0.3}
    current = {"a": 0.4, "b": 0.8, "new": 0.75}
    world = WorldState("s", 1, 100, model_state={PSYCHOLOGICAL_STATE_KEY: state})
    _kernel()(
        _controller(baselines={"new": baseline}), world, _view(previous), _deltas(current), 1439
    )
    written = world.model_state[PSYCHOLOGICAL_STATE_KEY]
    _answers(f"int-baseline-{baseline}", [_text(world.metric_state), _text(written)])
    assert type(written["agents"]["new"]["baseline_distress"]) is float


# ---- What the step refuses ---------------------------------------------------------------------


def _step(controller: Any, world: WorldState, view: Any, deltas: Any, minute: Any) -> None:
    """`_update_long_term_psychology` of a tick, on the people of the view."""
    _people_of(world, view)
    OfflineSimulationController._update_long_term_psychology(
        controller, world, view, deltas, minute
    )


@pytest.mark.parametrize(
    "change",
    [
        {"minute": 5},
        {"minute": True},
        {"current": {"a": 0.1}},
        {"current": {"a": 1.5, "b": 0.1}},
        {"previous": {"a": float("nan"), "b": 0.1}},
        {"threshold": 2.0},
    ],
    ids=[
        "the minute jumped",
        "a bool minute",
        "different people",
        "stress out of range",
        "previous out of range",
        "threshold out of range",
    ],
)
def test_a_bad_input_is_refused_and_changes_nothing(change: dict[str, Any]) -> None:
    """The deleted rules raised on these: a jump of the minute, a bool minute, other people, a
    stress or a threshold outside 0..1. No tick gives them."""
    arguments: dict[str, Any] = {
        "minute": 0,
        "previous": {"a": 0.1, "b": 0.1},
        "current": {"a": 0.7, "b": 0.1},
        "threshold": THRESHOLD,
        **change,
    }
    controller = _controller(threshold=arguments["threshold"])
    world = _world()
    before = _documents(world)
    view, deltas = _view(arguments["previous"]), _deltas(arguments["current"])
    with pytest.raises(KernelRefusal):
        _step(controller, world, view, deltas, arguments["minute"])
    assert _documents(world) == before


@pytest.mark.parametrize(
    "record",
    [
        {"daily_observed_minutes": 1439},
        {"daily_acute_sum": 5.0, "daily_observed_minutes": 2},
        {"daily_acute_sum": float("inf")},
        {"baseline_distress": 1.5},
        {"daily_high_distress_minutes": 3, "daily_observed_minutes": 2},
    ],
)
def test_a_record_the_deleted_rule_turned_down_is_refused(record: dict[str, Any]) -> None:
    """A record that validate_psychological_state turns down, now or after this minute."""
    state = _state("a", "b")
    state["agents"]["a"].update(record)
    world = _world(state)
    before = _documents(world)
    with pytest.raises(KernelRefusal):
        _step(_controller(), world, _view({"a": 0.1, "b": 0.1}), _deltas({"a": 0.1, "b": 0.1}), 0)
    assert _documents(world) == before


def test_a_bad_metric_is_refused() -> None:
    world = _world()
    world.metric_state["acute_distress_monitor:a:longest_run"] = -1.0
    assert _mirror(world, METRIC).has_row
    before = _documents(world)
    with pytest.raises(KernelRefusal, match="negative or not finite"):
        _step(_controller(), world, _view({"a": 0.1, "b": 0.1}), _deltas({"a": 0.1, "b": 0.1}), 0)
    assert _documents(world) == before


def test_a_model_without_long_term_state_has_nothing_to_do() -> None:
    """The models other than the two mean-reverting ones keep no long-term state."""
    world = _world()
    before = _documents(world)
    controller = _controller(version="life-state-feedback-v1")
    assert not hasattr(controller.psychology, "update_long_term_state")
    deltas = _deltas({"a": 0.1, "b": 0.1})
    _kernel()(controller, world, _view({"a": 0.1, "b": 0.1}), deltas, 0)
    assert _documents(world) == before


def test_another_model_with_long_term_state_is_refused() -> None:
    world = _world()
    controller = _controller(version="another-model")
    controller.psychology.update_long_term_state = lambda *arguments: None
    with pytest.raises(KernelRefusal, match="does not know"):
        _step(controller, world, _view({"a": 0.1, "b": 0.1}), _deltas({"a": 0.1, "b": 0.1}), 0)


@pytest.mark.parametrize(
    ("controller", "view", "deltas"),
    [
        (_controller(threshold=1), {"a": 0.1, "b": 0.1}, {"a": 0.1, "b": 0.1}),
        (_controller(), {"a": 0, "b": 0.1}, {"a": 0.1, "b": 0.1}),
        (_controller(), {"a": 0.1, "b": 0.1}, {"a": 1, "b": 0.1}),
        (
            _controller(baselines={"new": 1.5}),
            {"a": 0.1, "b": 0.1, "new": 0.1},
            {"a": 0.1, "b": 0.1, "new": 0.1},
        ),
    ],
)
def test_what_the_kernel_does_not_handle_is_unsupported_before_any_change(
    controller: Any, view: dict[str, Any], deltas: dict[str, Any]
) -> None:
    world = _world()
    before = _documents(world)
    with pytest.raises(native.Unsupported):
        _kernel()(controller, world, _view(view), _deltas(deltas), 0)
    assert _documents(world) == before


def test_two_deltas_for_one_person_are_refused() -> None:
    world = _world()
    deltas = (
        SimpleNamespace(agent_id="a", stress=0.1),
        SimpleNamespace(agent_id="a", stress=0.2),
    )
    with pytest.raises(KernelRefusal):
        _step(_controller(), world, _view({"a": 0.1, "b": 0.1}), deltas, 0)


def test_a_field_assigned_whole_stops_being_rust_and_the_step_is_refused() -> None:
    world = _world()
    assigned = {"x": 1.0}
    world.metric_state = assigned
    with pytest.raises(KernelRefusal):
        _step(_controller(), world, _view({"a": 0.1, "b": 0.1}), _deltas({"a": 0.1, "b": 0.1}), 0)
    assert world.metric_state is assigned


# ---- The validator -------------------------------------------------------------------------------


def _corruptions() -> list[Any]:
    def record_field(name: str, value: Any) -> Any:
        return lambda state: state["agents"]["a"].__setitem__(name, value)

    return [
        lambda state: state.__setitem__("schema_version", 2),
        lambda state: state.__setitem__("last_updated_minute", -2),
        lambda state: state.__setitem__("last_updated_minute", 1.5),
        lambda state: state.__delitem__("agents"),
        lambda state: state["agents"].__setitem__("", _record(0.1)),
        lambda state: state["agents"].__setitem__(1, _record(0.1)),
        lambda state: state["agents"].__setitem__("c", {"baseline_distress": 0.1}),
        record_field("baseline_distress", 1.5),
        record_field("baseline_distress", float("nan")),
        record_field("baseline_distress", True),
        record_field("long_term_distress", -0.1),
        record_field("daily_acute_sum", -0.1),
        record_field("daily_acute_sum", float("inf")),
        record_field("daily_acute_sum", 1.0),
        record_field("daily_observed_minutes", 1440),
        record_field("daily_observed_minutes", -1),
        record_field("daily_high_distress_minutes", 1),
        record_field("completed_days", -1),
    ]


@pytest.mark.parametrize("corrupt", _corruptions())
def test_the_validator_never_takes_what_the_python_rule_turns_down(corrupt: Any) -> None:
    plain = _state("a", "b")
    corrupt(plain)
    with pytest.raises(ValueError) as expected:
        validate_psychological_state(plain)

    world = _world()
    assert rust_validated(world, PSYCHOLOGICAL_STATE_KEY)
    corrupt(world.model_state[PSYCHOLOGICAL_STATE_KEY])
    assert not rust_validated(world, PSYCHOLOGICAL_STATE_KEY)
    with pytest.raises(ValueError) as raised:
        validate_psychological_state(world.model_state[PSYCHOLOGICAL_STATE_KEY])
    assert str(raised.value) == str(expected.value)


def test_the_validator_takes_what_the_python_rule_passes(generated_scenario: Any) -> None:
    def check(world: WorldState) -> None:
        assert rust_validated(world, PSYCHOLOGICAL_STATE_KEY)
        validate_psychological_state(world.model_state[PSYCHOLOGICAL_STATE_KEY])

    _run_ticks(generated_scenario, 10, check)


def test_a_corrupted_namespace_fails_the_world_with_the_exact_error(
    generated_scenario: Any,
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    world.model_state[PSYCHOLOGICAL_STATE_KEY]["schema_version"] = 99
    with pytest.raises(StateInvariantError) as raised:
        world.validate(validate_event_history=False)
    assert str(raised.value) == (
        "invalid psychological distress state: unsupported psychological distress state schema"
    )

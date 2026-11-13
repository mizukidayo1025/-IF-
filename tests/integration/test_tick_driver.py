"""The Rust driver of a tick (rules/tick.rs): what it runs, and what it refuses.

The shadows of benchmarks/rule_shadow go through Python entries, which the driver doesn't, so here
what a Run on the driver saved is compared with a digest recorded from it (`_digest`). The digests
were recorded when the phases' Python steps still ran the same Runs to the same bytes (#221).
See docs/rust-port/lanes/m6-driver.md.
"""

from __future__ import annotations

import ast
import hashlib
import json
import sys
from pathlib import Path
from typing import Any

import pytest
from support.paths import BENCHMARKS

from sheltersim import native
from sheltersim.core.engine import (
    TICK_PHASE_ORDER,
    TickEngine,
    TickExecutionError,
    TickPhase,
    TraceLevel,
)
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.experiments.performance import PhaseProfiler
from sheltersim.experiments.phases.driver import DRIVEN_RULES, DRIVEN_STEPS
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.inspection.traces import RunArtifactWriter
from sheltersim.llm.cache import ResponseTape
from sheltersim.llm.provider import LLMMode, LLMRouter, LLMSettings
from sheltersim.llm.runtime import EventTriggeredSocialPolicy, SocialRuntimeLimits

sys.path.insert(0, str(BENCHMARKS))

import tick_probe  # noqa: E402
from gate_scenarios import ScriptedProvider, _llm_settings, generated, with_rumors  # noqa: E402

ROOT = BENCHMARKS.parent


def _stats() -> tuple[dict[str, int], dict[str, int]]:
    counts, reasons = native.rules("tick").stats()
    return dict(counts), dict(reasons)


def _ticks() -> int:
    return _stats()[0]["ticks"]


def _build(
    scenario: Any,
    directory: Path,
    *,
    trace: TraceLevel = TraceLevel.OFF,
    router: LLMRouter | None = None,
    rng: Any = None,
) -> tuple[WorldState, OfflineSimulationController, TickEngine]:
    world = WorldState.from_scenario(scenario)
    artifacts = RunArtifactWriter(scenario, directory, trace_level=trace)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts, llm_router=router)
    return world, controller, TickEngine(world, phase_handler=controller, rng=rng)


def _saved(world: WorldState, controller: OfflineSimulationController) -> dict[str, Any]:
    return {
        "world": world.canonical_json(),
        "records": [raw.decode() for raw in world.event_log.items_bytes(0, len(world.event_log))],
        "pending": [event.to_dict() for event in world.event_queue.pending()],
        "counters": dict(world.id_counters),
        "metrics": controller.artifacts.metrics.to_dict(),
    }


def _run(scenario: Any, directory: Path, minutes: int, **options: Any) -> dict[str, Any]:
    world, controller, engine = _build(scenario, directory, **options)
    engine.run(minutes)
    return _saved(world, controller)


def _digest(value: Any) -> str:
    """Of what a Run saved, or of what its hooks were asked: the same on every OS, as the gate's
    hashes are."""
    text = json.dumps(value, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


@pytest.fixture
def short_scenario(generated_scenario: Any) -> Any:
    """Ends where these tests run to, so the Run's last minute is reached too."""
    return generated_scenario.model_copy(update={"duration_minutes": 240})


def test_the_rust_kernels_give_a_controller_a_driver() -> None:
    assert hasattr(OfflineSimulationController, "run_tick")


def _whole_run(
    scenario: Any, directory: Path, *, minutes: int = 180
) -> tuple[dict[str, Any], set[str]]:
    """Run on the driver, noting which entries of the two managers' loops Python called: the
    managers' `arbitrate` is the core's loop since lane P9-3b (its body-less `kernel_only` entry
    calls native/src/rules/toilet.rs and service.rs `arbitrate`)."""
    world, controller, engine = _build(scenario, directory)
    loops: set[str] = set()

    def watch(_frame: Any, event: str, argument: Any) -> None:
        if event == "c_call" and getattr(argument, "__name__", None) == "arbitrate":
            loops.add(f"{getattr(argument, '__module__', None)}.arbitrate")

    sys.setprofile(watch)
    try:
        for _ in range(minutes):
            engine.step()
    finally:
        sys.setprofile(None)
    return _saved(world, controller), loops


def test_the_driver_serves_independent_queues_itself(
    generated_scenario_with_split_queues: Any, tmp_path: Path
) -> None:
    before = _ticks()
    driven, loops = _whole_run(generated_scenario_with_split_queues, tmp_path / "driver")
    assert _ticks() == before + 180
    assert loops == set()
    requests = [
        event
        for event in driven["records"]
        if '"facility_queue_transition"' in event and '"service_started"' in event
    ]
    assert requests, "nobody was served, so the queues were not tried"
    assert _digest(driven) == "2e81691c83b3be1f"


def test_the_driver_keeps_the_view_the_next_phase_builds_on(
    generated_scenario: Any, tmp_path: Path
) -> None:
    """Only speed shows if it doesn't: the proposals' view is then built from the world."""
    _world, controller, engine = _build(generated_scenario, tmp_path)
    kept: list[bool] = []

    def watch(phase: TickPhase, before: bool) -> None:
        if not before and phase is TickPhase.UPDATE_AGENT_STATE:
            kept.append(controller._view_before_proposals is not None)
        if not before and phase is TickPhase.PROPOSE_ACTIONS:
            kept.append(controller._view_before_proposals is None)

    engine.phase_observer = watch
    before = _ticks()
    engine.run(2)
    assert _ticks() - before == 2
    assert kept == [True] * 4


class _OtherRng(CounterRNG):
    """Not plainly a CounterRNG, so what draws in Rust says Unsupported."""


def _without_contact_models(scenario: Any) -> Any:
    """The scenario without the social and information models, whose steps have no Python rule."""
    document = scenario.model_dump(mode="json")
    document["model_configs"]["social_propagation"]["enabled"] = False
    document["model_configs"]["information_spread"]["enabled"] = False
    return type(scenario).model_validate(document)


def test_without_the_contact_models_the_decisions_refuse_another_rng(
    generated_scenario: Any, tmp_path: Path
) -> None:
    """The decisions are the kernel alone too (lane L10): with the social and information models
    off, an RNG no Run has fails the tick at `_propose_due`, not in a Python rule."""
    scenario = _without_contact_models(generated_scenario)
    world, controller, engine = _build(scenario, tmp_path, rng=_OtherRng(scenario.seed))
    with pytest.raises(Exception) as raised:
        engine.step()
    chain = []
    error: BaseException | None = raised.value
    while error is not None:
        chain.append(error)
        error = error.__cause__ or error.__context__
    assert any(isinstance(item, KernelRefusal) for item in chain), chain
    assert any("propose_rows" in str(item) for item in chain), chain


def test_a_refusal_of_a_step_without_a_python_rule_fails_the_tick(
    generated_scenario: Any, tmp_path: Path
) -> None:
    """The social step is the kernel alone (segments.py, KERNEL_ONLY_SEGMENTS). An RNG no Run has
    is turned down, and the driver has no Python rule to run instead."""
    world, controller, engine = _build(
        generated_scenario, tmp_path, rng=_OtherRng(generated_scenario.seed)
    )
    with pytest.raises(Exception) as raised:
        engine.step()
    chain = []
    error: BaseException | None = raised.value
    while error is not None:
        chain.append(error)
        error = error.__cause__ or error.__context__
    assert any(isinstance(item, KernelRefusal) for item in chain), chain
    assert any("social_propagation" in str(item) for item in chain), chain


def _refused_with_nothing_changed(
    world: WorldState, controller: Any, engine: TickEngine, reason: str
) -> None:
    """The driver turns the tick down before anything changes, and nothing runs it instead."""
    state = world.state_digest()
    before = _ticks()
    with pytest.raises(KernelRefusal, match=f"^tick.run_tick refused: {reason}$"):
        engine.step()
    assert _ticks() == before
    assert world.state_digest() == state
    assert world.minute_index == 0


@pytest.mark.parametrize("step", DRIVEN_STEPS)
def test_a_patched_step_fails_the_tick(
    step: str, generated_scenario: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The driver would not call the patch: the tick fails rather than leaving it unseen."""
    world, controller, engine = _build(generated_scenario, tmp_path)
    original = getattr(OfflineSimulationController, step)

    def patched(self: Any, *arguments: Any, **options: Any) -> Any:
        return original(self, *arguments, **options)

    monkeypatch.setattr(OfflineSimulationController, step, patched)
    _refused_with_nothing_changed(world, controller, engine, "a step of the controller is patched")


@pytest.mark.parametrize(
    "holder, name", DRIVEN_RULES, ids=[f"{item[0].__name__}.{item[1]}" for item in DRIVEN_RULES]
)
def test_a_patched_rule_fails_the_tick(
    holder: Any, name: str, generated_scenario: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    world, controller, engine = _build(generated_scenario, tmp_path)
    original = getattr(holder, name)

    def patched(*arguments: Any, **options: Any) -> Any:
        return original(*arguments, **options)

    monkeypatch.setattr(holder, name, patched)
    _refused_with_nothing_changed(world, controller, engine, "a rule a step calls is patched")


def test_a_step_patched_on_the_controller_itself_fails_the_tick(
    generated_scenario: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    world, controller, engine = _build(generated_scenario, tmp_path)
    calls = []
    original = controller._propose_due

    def counted(*arguments: Any) -> Any:
        calls.append(arguments)
        return original(*arguments)

    monkeypatch.setattr(controller, "_propose_due", counted)
    _refused_with_nothing_changed(world, controller, engine, "a step of this controller is patched")
    assert calls == []


def test_a_manager_of_another_class_fails_the_tick(generated_scenario: Any, tmp_path: Path) -> None:
    world, controller, engine = _build(generated_scenario, tmp_path)

    class Other(type(controller.toilet_service)):
        pass

    controller.toilet_service.__class__ = Other
    _refused_with_nothing_changed(world, controller, engine, "another class of toilet service")


def test_a_dispatch_its_own_checks_refuse_fails_the_tick_as_the_phase_does(
    generated_scenario: Any, tmp_path: Path
) -> None:
    """The dispatch phase's checks before its first event: the refusal its entry gives."""
    world, controller, engine = _build(generated_scenario, tmp_path)

    class Other(type(controller.event_dispatcher)):
        pass

    controller.event_dispatcher = Other(dict(controller.event_dispatcher.handlers))
    state = world.state_digest()
    before = _ticks()
    with pytest.raises(
        KernelRefusal, match="^dispatch.run_phase refused: another kind of dispatcher$"
    ):
        engine.step()
    with pytest.raises(
        KernelRefusal, match="^dispatch.run_phase refused: another kind of dispatcher$"
    ):
        controller._run_dispatch_phase(world, 0)
    assert _ticks() == before
    assert world.state_digest() == state


def test_some_phases_of_a_tick_run_alone_in_the_tick_s_order(
    generated_scenario: Any, tmp_path: Path
) -> None:
    """For a test that steps a part of a tick: the phases asked for, as the whole tick runs them."""
    world, controller, whole = _build(generated_scenario, tmp_path / "whole")
    seen: list[tuple[TickPhase, bool]] = []
    part, part_controller, _engine = _build(generated_scenario, tmp_path / "part")
    asked = [TickPhase.UPDATE_AGENT_STATE, TickPhase.DISPATCH_DUE_EVENTS]
    before = _ticks()
    part_controller.run_tick(
        part, 0, CounterRNG(part.seed), lambda phase, stage: seen.append((phase, stage)), asked
    )
    assert _ticks() - before == 1
    assert seen == [
        (TickPhase.DISPATCH_DUE_EVENTS, True),
        (TickPhase.DISPATCH_DUE_EVENTS, False),
        (TickPhase.UPDATE_AGENT_STATE, True),
        (TickPhase.UPDATE_AGENT_STATE, False),
    ]
    # The whole tick's world after its update phase is the part's.
    after_update: list[str] = []

    def watch(phase: TickPhase, before: bool) -> None:
        if phase is TickPhase.UPDATE_AGENT_STATE and not before:
            after_update.append(world.state_digest())

    whole.phase_observer = watch
    whole.step()
    assert after_update == [part.state_digest()]
    assert part.minute_index == 0
    with pytest.raises(ValueError, match='no phase of a tick is called "sleep"'):
        part_controller.run_tick(part, 0, CounterRNG(part.seed), None, ["sleep"])


@pytest.mark.parametrize("level", [TraceLevel.SUMMARY, TraceLevel.FULL])
def test_a_trace_runs_on_the_driver_and_keeps_its_files(
    level: TraceLevel, generated_scenario: Any, tmp_path: Path
) -> None:
    world, controller, engine = _build(generated_scenario, tmp_path, trace=level)
    before = _ticks()
    engine.run(3)
    assert _ticks() - before == 3
    driven = _saved(world, controller)
    controller.artifacts.finalize(world)
    files = {name: (tmp_path / name).read_text("utf-8") for name in TRACE_FILES}
    assert all(files.values())
    assert _digest([driven, files]) == TRACE_DIGESTS[level]


# The files of a trace that the kernels write the documents of.
TRACE_FILES = (
    "decisions.jsonl",
    "information_contacts.jsonl",
    "psychology_contributions.jsonl",
    "social_contacts.jsonl",
)
TRACE_DIGESTS = {TraceLevel.SUMMARY: "d4cffb2d2c539c9a", TraceLevel.FULL: "e179df436790686a"}


def test_a_trace_of_the_engine_s_phases_runs_on_the_driver(
    generated_scenario: Any, tmp_path: Path
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    artifacts = RunArtifactWriter(generated_scenario, tmp_path, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    seen: list[Any] = []
    watched: list[Any] = []
    engine = TickEngine(
        world,
        phase_handler=OfflineSimulationController(world, artifacts),
        trace_level=TraceLevel.FULL,
        trace_sink=seen.append,
    )
    engine.phase_observer = lambda phase, before: watched.append((phase, before))
    before = _ticks()
    engine.run(2)
    assert _ticks() - before == 2
    assert [item.phase for item in seen] == list(TICK_PHASE_ORDER) * 2
    assert [item.minute_index for item in seen] == [0] * 7 + [1] * 7
    assert watched == [(phase, stage) for phase in TICK_PHASE_ORDER for stage in (True, False)] * 2


class _KeepingProvider(ScriptedProvider):
    """Keeps what each call asks, and never leaves this process."""

    def __init__(self) -> None:
        self.asked: list[str] = []

    def complete(self, *, model_id: str, task: Any, **options: Any) -> Any:
        self.asked.append(task.input_text)
        return super().complete(model_id=model_id, task=task, **options)


def _stressed(controller: OfflineSimulationController, world: WorldState) -> None:
    """Each person under stress and the daily caps open, as the gate's `_stressed_run` does."""
    for agent in world.agents.values():
        agent.stress = 0.9
    controller.social_policy.limits = SocialRuntimeLimits(
        cooldown_minutes=5, per_agent_per_day=40, global_per_day=120, complaint_wait_minutes=5
    )
    # The complaints' threshold is the Run's (`Policies`, lane P8 b3): read it again.
    controller._run_core.refresh_policies(controller)


def test_a_router_that_can_call_a_provider_runs_on_the_driver(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    minutes = 240
    scenario = with_rumors(generated(evacuees=24, minutes=minutes, staff_count=1))
    calls: list[tuple[str, int, tuple[Any, ...]]] = []
    for name in ("consider_support_request", "consider_complaint_response"):
        original = getattr(EventTriggeredSocialPolicy, name)

        def recorded(self: Any, world: Any, *, _name: str = name, _rule: Any = original, **o: Any):
            calls.append((_name, world.minute_index, tuple(sorted(o.items()))))
            return _rule(self, world, **o)

        monkeypatch.setattr(EventTriggeredSocialPolicy, name, recorded)

    def run(directory: Path, router: LLMRouter) -> tuple[dict[str, Any], list[Any]]:
        calls.clear()
        world, controller, engine = _build(scenario, directory, router=router)
        _stressed(controller, world)
        engine.run(minutes)
        return _saved(world, controller), list(calls)

    before = _ticks()
    provider = _KeepingProvider()
    record = LLMRouter(_llm_settings(LLMMode.RECORD), provider=provider, tape=ResponseTape())
    driven, driven_calls = run(tmp_path / "driven", record)
    assert _ticks() - before == minutes
    assert provider.asked, "no Provider call was reached"
    assert _digest([driven, driven_calls, provider.asked]) == "5fff469501f37f5c"

    # The same recorded responses, from the tape alone: no Provider is given.
    before = _ticks()
    replay = LLMRouter(
        _llm_settings(LLMMode.REPLAY), tape=ResponseTape.from_dict(record.tape.to_dict())
    )
    replayed, replayed_calls = run(tmp_path / "replay", replay)
    assert _ticks() - before == minutes
    assert replayed == driven
    assert replayed_calls == driven_calls


def test_an_answer_the_schema_refuses_falls_back_on_the_driver(
    tmp_path: Path,
) -> None:
    """LLM output stays untrusted on either path: the router checks it, before any rule reads it."""
    minutes = 240
    scenario = with_rumors(generated(evacuees=24, minutes=minutes, staff_count=1))

    class Refused(_KeepingProvider):
        @staticmethod
        def _answer(task: Any) -> str:
            return '{"priority":"run this code","rationale":"no"}'

    def run(directory: Path) -> tuple[dict[str, Any], list[str]]:
        provider = Refused()
        router = LLMRouter(_llm_settings(LLMMode.RECORD), provider=provider, tape=ResponseTape())
        world, controller, engine = _build(scenario, directory, router=router)
        _stressed(controller, world)
        engine.run(minutes)
        return _saved(world, controller), provider.asked

    before = _ticks()
    driven, asked = run(tmp_path / "driven")
    assert _ticks() - before == minutes
    assert asked
    assert "run this code" not in driven["world"]
    assert _digest([driven, asked]) == "82d47d3e6433f0c4"


def test_the_hooks_of_an_offline_router_run_on_the_driver(
    generated_scenario: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    minutes = generated_scenario.duration_minutes
    calls: list[tuple[str, int, tuple[Any, ...]]] = []
    for name in ("consider_support_request", "consider_complaint_response"):
        original = getattr(EventTriggeredSocialPolicy, name)

        def recorded(self: Any, world: Any, *, _name: str = name, _rule: Any = original, **o: Any):
            calls.append((_name, world.minute_index, tuple(sorted(o.items()))))
            return _rule(self, world, **o)

        monkeypatch.setattr(EventTriggeredSocialPolicy, name, recorded)

    def run(directory: Path) -> tuple[dict[str, Any], list[Any]]:
        calls.clear()
        settings = LLMSettings(
            mode=LLMMode.OFFLINE,
            max_api_calls=1,
            max_input_tokens=1024,
            max_output_tokens=64,
            budget_usd=0.0,
        )
        saved = _run(generated_scenario, directory, minutes, router=LLMRouter(settings))
        return saved, list(calls)

    before = _ticks()
    driven, driven_calls = run(tmp_path / "driven")
    assert _ticks() - before == minutes
    assert any(call[0] == "consider_support_request" for call in driven_calls)
    assert _digest([driven, driven_calls]) == "bd893be459a84081"


class _Seen:
    def __init__(self) -> None:
        self.phases: list[tuple[int, str, bool]] = []

    def engine_started(self, engine: int, world: WorldState) -> None:
        pass

    def before_phase(self, engine: int, world: Any, minute: int, phase: TickPhase) -> None:
        self.phases.append((minute, phase.value, True))

    def after_phase(self, engine: int, world: Any, minute: int, phase: TickPhase) -> None:
        self.phases.append((minute, phase.value, False))


def test_a_tick_probe_watches_the_driver_s_phases(generated_scenario: Any, tmp_path: Path) -> None:
    expected = [
        (minute, phase.value, stage)
        for minute in range(2)
        for phase in TICK_PHASE_ORDER
        for stage in (True, False)
    ]
    seen = _Seen()
    undo = tick_probe.install([seen])
    before = _ticks()
    try:
        watched = _run(generated_scenario, tmp_path / "watched", 2)
    finally:
        undo()
    assert _ticks() - before == 2
    assert seen.phases == expected
    assert watched == _run(generated_scenario, tmp_path / "unwatched", 2)


def test_the_phase_profiler_leaves_the_driver_running(
    generated_scenario: Any, tmp_path: Path
) -> None:
    profiler = PhaseProfiler()
    _world, _controller, engine = _build(generated_scenario, tmp_path)
    engine.phase_observer = profiler.observe
    before = _ticks()
    engine.run(2)
    assert _ticks() - before == 2
    report = profiler.report()
    assert {row["phase"] for row in report["phases"]} == {str(phase) for phase in TICK_PHASE_ORDER}
    assert all(row["calls"] == 2 for row in report["phases"])


def test_the_driver_says_what_the_engine_says_of_a_phase_that_moves_the_clock(
    generated_scenario: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    world, controller, engine = _build(generated_scenario, tmp_path)
    capture = controller.artifacts.capture_metric

    def moves_the_clock(*arguments: Any, **options: Any) -> Any:
        result = capture(*arguments, **options)
        world.minute_index += 1
        return result

    monkeypatch.setattr(controller.artifacts, "capture_metric", moves_the_clock)
    before = _ticks()
    with pytest.raises(TickExecutionError, match="engine-owned clock"):
        engine.step()
    assert _ticks() - before == 1


def test_an_error_of_a_rule_comes_through_the_driver_as_itself(
    generated_scenario: Any, tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    world, controller, engine = _build(generated_scenario, tmp_path)

    def fails(*_arguments: Any, **_options: Any) -> None:
        raise ValueError("this rule's own words")

    monkeypatch.setattr(controller.artifacts, "capture_metric", fails)
    with pytest.raises(ValueError, match="this rule's own words"):
        engine.step()


def test_a_world_with_another_controller_is_refused_with_nothing_changed(
    generated_scenario: Any, tmp_path: Path
) -> None:
    world, controller, engine = _build(generated_scenario, tmp_path / "first")
    other = WorldState.from_scenario(generated_scenario)
    state = world.state_digest()
    with pytest.raises(
        KernelRefusal, match="^tick.run_tick refused: the controller is bound to another world$"
    ):
        controller.run_tick(other, 0, engine.rng, None)
    assert world.state_digest() == state


# What each phase's steps are called in the core's phases (native/core/src/rules/tick/), in the
# order the phase's mixin gives in its docstring.
_PHASE_STEPS = {
    ("update", "UpdatePhase", "pub fn update<"): {
        "advance_movement": "update_movement(tick)",
        "_view_after_movement": "view_after_movement(tick)",
        "_psychology_update": "update_psychology(tick,",
        "_social_propagation": "update_social(tick,",
        "_record_psychology_trace": "record_trace(tick,",
        "_update_long_term_psychology": "update_long_term(tick,",
        "_apply_state_deltas": "apply_deltas(tick,",
    },
    ("propose", "ProposePhase", "pub fn propose<"): {
        "_view_for_proposals": "view_for_proposals(tick)",
        "_propose_due": "due(tick,",
        "_observed_state": "observed(tick,",
        "_apply_effect": "apply_effect(tick,",
        "_record_outcome": "record_outcome(tick,",
    },
    ("arbitrate", "ArbitratePhase", "pub fn arbitrate<"): {
        "update_staff_dynamics": "staff_dynamics(tick)",
        "update_staff_operations": "update_operations(tick)",
        "toilet_service.arbitrate": "crate::rules::toilet::arbitrate(tick)",
        "resource_service.arbitrate": "crate::rules::service::arbitrate(tick)",
        "_assign_staff_support": "// `_assign_staff_support`.",
        "assign_staff_operations": "assign_operations(tick)",
        "_consider_due_complaints": "consider_due_complaints(tick)",
        "schedule_staff_operation_relief": "relief(tick)",
    },
    ("sample", "SamplePhase", "pub fn sample<"): {
        "_sample_information_contacts": "sample_information(tick)",
        "_sample_proximity_contacts": "sample_proximity(tick)",
    },
}

CORE_TICK = ROOT / "native/core/src/rules/tick"


@pytest.mark.parametrize(
    "module, owner, rust_function", _PHASE_STEPS, ids=[key[0] for key in _PHASE_STEPS]
)
def test_the_driver_takes_a_phase_s_steps_in_the_order_its_mixin_gives(
    module: str, owner: str, rust_function: str
) -> None:
    """Most scenarios can't see two steps swapped when neither writes in the same minute."""
    source = (ROOT / "src/sheltersim/experiments/phases" / f"{module}.py").read_text("utf-8")
    phase = next(
        node
        for node in ast.parse(source).body
        if isinstance(node, ast.ClassDef) and node.name == owner
    )
    contract = ast.get_docstring(phase) or ""
    steps = _PHASE_STEPS[(module, owner, rust_function)]
    in_python = [contract.index(f"`{name}") for name in steps]
    assert in_python == sorted(in_python)
    rust = (CORE_TICK / f"{module}.rs").read_text("utf-8")
    start = rust.index(rust_function)
    end = rust.index("\n}\n", start)
    places = [rust.index(steps[name], start, end) for name in steps]
    assert places == sorted(places)


def test_the_driver_takes_the_phases_in_the_engine_s_order() -> None:
    """The PyO3 driver calls the core's `run_phases`, which runs the phases in `Phase::ORDER`,
    which names `TICK_PHASE_ORDER`'s members in their order; the core's `run_tick` runs them all."""
    driver = (ROOT / "native/src/rules/tick.rs").read_text("utf-8")
    run = driver.index("fn run_tick<'py>(")
    assert driver.index("core::run_phases(&mut tick, &phases)", run) > run
    host = (ROOT / "native/core/src/host.rs").read_text("utf-8")
    order = host.index("pub const ORDER: [Phase; 7] = [")
    names = host.index("pub fn name(self)", order)
    for phase in TICK_PHASE_ORDER:
        assert f'=> "{phase.value}"' in host[names:]
    places = [host.index(f"Phase::{_camel(phase.value)},", order) for phase in TICK_PHASE_ORDER]
    assert places == sorted(places)
    phases = (CORE_TICK / "mod.rs").read_text("utf-8")
    whole = phases.index("pub fn run_tick<")
    assert phases.index("run_phases(tick, &Phase::ORDER)", whole) > whole
    start = phases.index("pub fn run_phases<")
    assert phases.index("for phase in Phase::ORDER {", start) > start
    bodies = {
        TickPhase.DISPATCH_DUE_EVENTS: "Phase::DispatchDueEvents => crate::rules::dispatch::run(",
        TickPhase.COMPLETE_SCHEDULED_TASKS: "Phase::CompleteScheduledTasks => {}",
        TickPhase.UPDATE_AGENT_STATE: "Phase::UpdateAgentState => update::update(",
        TickPhase.PROPOSE_ACTIONS: "Phase::ProposeActions => propose::propose(",
        TickPhase.ARBITRATE_ACTIONS: "Phase::ArbitrateActions => arbitrate::arbitrate(",
        TickPhase.SAMPLE_ENDOGENOUS_EVENTS: "Phase::SampleEndogenousEvents => sample::sample(",
        TickPhase.COLLECT_METRICS: "Phase::CollectMetrics => metrics::metrics(",
    }
    assert all(phases.find(body, start) > 0 for body in bodies.values())
    assert list(bodies) == list(TICK_PHASE_ORDER)


def _camel(value: str) -> str:
    return "".join(part.title() for part in value.split("_"))


# The digest of `test_the_arbitrate_loops_give_the_world_of_the_driver_s_loops`, taken from the
# build before the arbitrate loops moved to the core (main 60806a37, Issue #152 lane P9-2a).
ARBITRATE_DIGEST = "12e725ac126866cf"


def test_the_arbitrate_loops_give_the_world_of_the_driver_s_loops(
    generated_scenario_with_split_queues: Any,
) -> None:
    """The toilets' and the resource service's loops (native/core/src/rules/toilet.rs and
    service.rs `arbitrate`), with independent queues and their stall records: the world after
    each arbitrate phase of 240 minutes, against the digest the PyO3 driver's loops gave."""
    import hashlib

    from support.world import world_digest

    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.timeline import _MetricSink

    document = generated_scenario_with_split_queues.model_dump(mode="json")
    document["duration_minutes"] = 240
    world = WorldState.from_scenario(
        type(generated_scenario_with_split_queues).model_validate(document)
    )
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    digests: list[str] = []
    queued: set[str] = set()

    def watch(phase: TickPhase, before: bool) -> None:
        if phase is TickPhase.ARBITRATE_ACTIONS and not before:
            requests = world.model_state["facility_queues_v1"]["requests"].values()
            queued.update(request["facility_id"] for request in requests)
            service = world.model_state["resource_service_v1"]
            stalls = [
                json.dumps(record, sort_keys=True) for record in service["queue_stall_history"]
            ]
            digests.append(f"{world_digest(world)} {len(world.event_log)} {stalls}")

    engine.phase_observer = watch
    before = _ticks()
    engine.run(240)
    assert _ticks() - before == 240
    assert {"toilet_01", "meal_distribution_main"} <= queued
    assert world.model_state["resource_service_v1"]["queue_stall_history"], "no stall recorded"
    digest = hashlib.sha256("\n".join(digests).encode("utf-8")).hexdigest()[:16]
    assert digest == ARBITRATE_DIGEST

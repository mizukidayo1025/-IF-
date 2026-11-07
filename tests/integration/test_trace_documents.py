"""The trace documents in Rust (rules/trace.rs), against the Python rules' documents, bit by bit.

A Run with a trace goes through the driver and its kernels. Here each model writes its files on the
driver and with the kernels removed, and random worlds reach values no scenario does. Decision D1 of
docs/rust-port/step4-plan.md: the documents keep their shape. See docs/rust-port/lanes/l6-trace.md.
"""

from __future__ import annotations

import hashlib
import json
import os
import random
import sys
from dataclasses import fields
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from support.paths import BENCHMARKS
from support.python_rules import without_kernels

from sheltersim import native
from sheltersim.core.engine import TraceLevel
from sheltersim.core.world import WorldState
from sheltersim.domain.models import SupportRequestState, SupportRequestStatus
from sheltersim.experiments.phases.update import UpdatePhase
from sheltersim.experiments.runner import OfflineSimulationController, run_scenario
from sheltersim.inspection.traces import RunArtifactWriter
from sheltersim.policies.alternatives import BoundedStressPolicy
from sheltersim.policies.psychology import (
    IntegratedDistressLifeStatePolicy,
    LifeStatePsychologyPolicy,
    MeanRevertingDistressPolicy,
    PsychologyParameters,
    SimplePsychologyPolicy,
)
from sheltersim.policies.registry import BodyDynamicsFormula

sys.path.insert(0, str(BENCHMARKS))

from gate_scenarios import _model_scenario  # noqa: E402

TRACE_FILES = (
    "psychology_contributions.jsonl",
    "psychology_contributions_hourly.jsonl",
    "social_contacts.jsonl",
    "information_contacts.jsonl",
    "decisions.jsonl",
)
MODELS = (
    ("psychology", "causal-stress-v1"),
    ("psychology", "bounded-stress-v1"),
    ("psychology", "mean-reverting-distress-v1"),
    ("psychology", "life-state-feedback-v1"),
    ("psychology", "mean-reverting-life-state-v1"),
    ("social", "sparse-family-zone-contact-v1"),
    ("social", "expressiveness-channel-openness-v1"),
    ("social", "degroot-contact-v1"),
    ("information", "sparse-information-contact-v1"),
    ("information", "hazard-information-v1"),
    ("information", "evidence-aware-rumor-v1"),
)


# What the deleted Python steps wrote (`_social_propagation` and `_sample_information_contacts` of
# experiments/phases at 2d8fb46, on policies/social.py, information.py and alternatives.py), as
# sha256 of the files, recorded once on Linux. With SHELTERSIM_RECORD_CONTACT_RUNS set, the tests
# ran those Python steps and wrote their digests to that file instead.
ANSWERS: dict[str, str] = {
    "information-evidence-aware-rumor-v1-full": (
        "fb778b1f2efe73f30f329d7c607ad89e7525e49b953a07efd276b28a543940e7"
    ),
    "information-hazard-information-v1-full": (
        "d12a6e4ef3f0e6b6e88ae908236c824753cab76b5dbfcd95188c3a56c1ee2bae"
    ),
    "information-hazard-information-v1-summary": (
        "4f4e45a4960be69011340e45aed649bc80142d2d019c0b3ff345c7b54755fc30"
    ),
    "information-sparse-information-contact-v1-full": (
        "f6074fdb6f36d2b32872a379fe6834a350008e6a44613e771b558e45a8af90ea"
    ),
    "psychology-bounded-stress-v1-full": (
        "e18873bd88512855e5ad8605bdac008b00e135f767e9a9235fb8d9ed41e2c486"
    ),
    "psychology-causal-stress-v1-full": (
        "e18873bd88512855e5ad8605bdac008b00e135f767e9a9235fb8d9ed41e2c486"
    ),
    "psychology-causal-stress-v1-summary": (
        "4aba58d720f035c8365cddf8fabca84cf1a37e300062cf0855b33906cd2f3f9c"
    ),
    "psychology-life-state-feedback-v1-full": (
        "08c928a34f1ba7755e78bae1f960fd7c7f484a6a8ebbd27b922d077749d57749"
    ),
    "psychology-mean-reverting-distress-v1-full": (
        "e18873bd88512855e5ad8605bdac008b00e135f767e9a9235fb8d9ed41e2c486"
    ),
    "psychology-mean-reverting-life-state-v1-full": (
        "fb778b1f2efe73f30f329d7c607ad89e7525e49b953a07efd276b28a543940e7"
    ),
    "psychology-mean-reverting-life-state-v1-summary": (
        "b3b45b0cd5f92ac8a47ef4978ef1996eae73cf0f4dc8842e7e5294aec751d25a"
    ),
    "social-degroot-contact-v1-full": (
        "45bd3d00cc58361b56656cf994cbafc1b3c8717ce8d147cf56344888a3955c6e"
    ),
    "social-degroot-contact-v1-summary": (
        "cf9a42ccbbfcac3504ca23786a829a1e2d0086a25c13ee78f69b7cbeb676b1b3"
    ),
    "social-expressiveness-channel-openness-v1-full": (
        "fb778b1f2efe73f30f329d7c607ad89e7525e49b953a07efd276b28a543940e7"
    ),
    "social-sparse-family-zone-contact-v1-full": (
        "405482147be37d931f2ceadbc3b887a59da7ad0c6b7498ad04a143b5b2c6d688"
    ),
}
_RECORD = os.environ.get("SHELTERSIM_RECORD_CONTACT_RUNS")


def _answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    if _RECORD:
        with open(_RECORD, "a", encoding="utf-8") as file:
            file.write(json.dumps({name: digest.hexdigest()}) + "\n")
        return
    if sys.platform == "linux":
        assert digest.hexdigest() == ANSWERS[name], name


def _python_contact_steps(patch: pytest.MonkeyPatch) -> None:
    """Only while recording: the deleted Python steps of the contact models."""
    from sheltersim.experiments.phases.sample import SamplePhase
    from sheltersim.experiments.phases.update import UpdatePhase
    from sheltersim.experiments.runner import OfflineSimulationController

    patch.setattr(
        OfflineSimulationController, "_social_propagation", UpdatePhase._social_propagation
    )
    patch.setattr(
        OfflineSimulationController,
        "_sample_information_contacts",
        SamplePhase._sample_information_contacts,
    )


def _ticks() -> int:
    return native.rules("tick").stats()[0]["ticks"]


def _files(directory: Path) -> dict[str, bytes | None]:
    return {
        name: (directory / name).read_bytes() if (directory / name).exists() else None
        for name in TRACE_FILES
    }


@pytest.mark.parametrize(
    ("domain", "model", "level"),
    [
        *((domain, model, TraceLevel.FULL) for domain, model in MODELS),
        # A summary keeps a sample, the same whichever model made the list.
        ("psychology", "causal-stress-v1", TraceLevel.SUMMARY),
        ("psychology", "mean-reverting-life-state-v1", TraceLevel.SUMMARY),
        ("social", "degroot-contact-v1", TraceLevel.SUMMARY),
        ("information", "hazard-information-v1", TraceLevel.SUMMARY),
    ],
    ids=str,
)
def test_a_run_with_a_trace_writes_the_bytes_of_the_python_rules(
    domain: str, model: str, level: TraceLevel, tmp_path: Path
) -> None:
    """The psychology and decision files against the Python rules of a Run without the kernels;
    the contact files, whose Python steps are gone, against what those steps wrote."""
    scenario = _model_scenario(domain, model)
    before = _ticks()
    with pytest.MonkeyPatch.context() as patch:
        if _RECORD:
            _python_contact_steps(patch)
        run_scenario(scenario, tmp_path / "driver", trace_level=level)
    if not _RECORD:
        assert _ticks() - before == scenario.duration_minutes
    driven = _files(tmp_path / "driver")
    with pytest.MonkeyPatch.context() as patch:
        without_kernels(patch)
        run_scenario(scenario, tmp_path / "python", trace_level=level)
    assert driven["psychology_contributions.jsonl"]
    contacts = ("social_contacts.jsonl", "information_contacts.jsonl")
    for name, raw in _files(tmp_path / "python").items():
        if name not in contacts:
            assert driven[name] == raw, name
    _answers(
        f"{domain}-{model}-{level.value}",
        [(driven[name] or b"").decode("utf-8") for name in contacts],
    )


def _text(value: Any) -> str:
    return json.dumps([getattr(value, item.name) for item in fields(value)], ensure_ascii=False)


def _shuffle(world: WorldState, draws: random.Random) -> None:
    extremes = (0.0, 1.0)
    zones = sorted(world.zones)
    people = list(world.agents.values())
    for number, agent in enumerate(people):
        agent.zone_id = draws.choice(zones)
        for name in ("toilet_urgency", "hunger", "fatigue", "hydration_need"):
            setattr(agent.needs, name, draws.random() if number % 3 else draws.choice(extremes))
        agent.stress = draws.random() if number % 3 else draws.choice(extremes)
    for number, agent in enumerate(people[:6]):
        status = list(SupportRequestStatus)[number % 4]
        request = SupportRequestState(f"R{number}", agent.id, 0, 10, status=status)
        world.support_requests[request.id] = request
        agent.support_request_id = request.id


def _policies(draws: random.Random, ids: list[str]) -> list[Any]:
    def rate() -> float:
        return draws.choice((0.0, draws.random() * 0.25))

    parameters = PsychologyParameters(
        toilet_growth=rate(),
        hunger_growth=rate(),
        awake_fatigue_growth=rate(),
        resting_fatigue_recovery=rate(),
        sleeping_fatigue_recovery=rate(),
        stress_pressure=rate(),
        queued_stress=rate(),
        resting_stress_recovery=rate(),
        sleeping_stress_recovery=rate(),
        need_pressure_threshold=draws.random(),
        support_wait_stress=rate(),
        natural_recovery_rate_per_minute=rate(),
        baseline_distress_by_agent={ids[0]: draws.random()},
    )
    body = {"uncertainty_drift_per_minute": draws.random() * 0.01}
    formula = BodyDynamicsFormula("body-feedback-complete-v1", "S")
    return [
        SimplePsychologyPolicy(parameters),
        SimplePsychologyPolicy(parameters, include_direct_rest_stress_recovery=False),
        BoundedStressPolicy(parameters),
        MeanRevertingDistressPolicy(parameters),
        LifeStatePsychologyPolicy(parameters, body_config=body),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=formula),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=formula),
    ]


class _Keeping:
    """What a sink is given, as text."""

    def __init__(self, level: TraceLevel) -> None:
        self.trace_level = level
        self.documents: list[str] = []

    def record_psychology_trace(self, trace: Any, *, agent_contributions: Any = ()) -> None:
        self.documents.append(json.dumps([trace, list(agent_contributions)], ensure_ascii=False))


@pytest.mark.parametrize("seed", range(6))
@pytest.mark.parametrize(
    "level", [TraceLevel.SUMMARY, TraceLevel.FULL], ids=lambda level: level.value
)
def test_the_rows_and_the_document_of_each_model_are_python_s(
    generated_scenario: Any, tmp_path: Path, seed: int, level: TraceLevel
) -> None:
    rules = native.rules("psychology")
    record = native.rules("trace").record_psychology_trace
    draws = random.Random(seed)
    world = WorldState.from_scenario(generated_scenario)
    artifacts = RunArtifactWriter(generated_scenario, tmp_path, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    _shuffle(world, draws)
    view = controller._view_after_movement(world)
    for policy in _policies(draws, sorted(world.agents)):
        controller.psychology = policy
        controller.artifacts = _Keeping(level)
        result = rules.update_psychology(controller, view, 0)
        python = policy.explain_update(view, 0)
        assert [_text(trace) for trace in result.traces] == [
            _text(trace) for trace in python.traces
        ]
        assert result.policy_version == python.policy_version
        # What the social step does between the two.
        final = tuple(
            delta.with_stress(draws.random()) if number % 2 else delta
            for number, delta in enumerate(result.deltas)
        )
        record(controller, result, final, 0)
        UpdatePhase._record_psychology_trace(controller, python, final, 0)
        ours, theirs = controller.artifacts.documents
        assert ours == theirs


def _int_policies(ids: list[str]) -> list[Any]:
    """Ints a Scenario may write (per-person rates and baselines, body rates of `0` or `1`)."""
    parameters = PsychologyParameters(
        toilet_growth_by_agent={ids[0]: 0},
        natural_recovery_rate_by_agent={ids[1]: 0},
        baseline_distress_by_agent={ids[0]: 1, ids[1]: 0},
        hydration_growth=0,
    )
    body = {
        "discontent_stress_rate": 1,
        "awake_sleep_debt_minutes_per_minute": 1,
        "reassurance_decay_per_minute": 0,
        "sleep_debt_stress_rate_by_agent": {ids[0]: 1},
    }
    formula = BodyDynamicsFormula("body-feedback-complete-v1", "S")
    return [
        MeanRevertingDistressPolicy(parameters),
        LifeStatePsychologyPolicy(parameters, body_config=body),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=formula),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=formula),
    ]


@pytest.mark.parametrize(
    "level", [TraceLevel.SUMMARY, TraceLevel.FULL], ids=lambda level: level.value
)
def test_int_values_of_a_scenario_give_the_python_rule_s_document(
    generated_scenario: Any, tmp_path: Path, level: TraceLevel
) -> None:
    """A separated household's pressure is the int `discontent_stress_rate`: the Python rule kept
    it an int in `life_state_before` and `after`, and JSON writes `1`, not `1.0`."""
    rules = native.rules("psychology")
    record = native.rules("trace").record_psychology_trace
    world = WorldState.from_scenario(generated_scenario)
    artifacts = RunArtifactWriter(generated_scenario, tmp_path, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    _shuffle(world, random.Random(3))
    view = controller._view_after_movement(world)
    separated = 0
    for policy in _int_policies(sorted(world.agents)):
        controller.psychology = policy
        controller.artifacts = _Keeping(level)
        result = rules.update_psychology(controller, view, 0)
        python = policy.explain_update(view, 0)
        assert [_text(trace) for trace in result.traces] == [
            _text(trace) for trace in python.traces
        ]
        record(controller, result, result.deltas, 0)
        UpdatePhase._record_psychology_trace(controller, python, result.deltas, 0)
        ours, theirs = controller.artifacts.documents
        assert ours == theirs
        separated += ours.count(
            '"family_separation_pressure": {"before": 1, "after": 1, "delta": 0}'
        )
    assert separated > 0


def test_a_python_result_stays_with_the_python_step(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = SimpleNamespace(
        psychology=SimplePsychologyPolicy(), artifacts=_Keeping(TraceLevel.SUMMARY)
    )
    result = SimpleNamespace(traces=(), deltas=())
    with pytest.raises(native.Unsupported, match="Python rule's"):
        native.rules("trace").record_psychology_trace(controller, result, (), world.minute_index)
    assert controller.artifacts.documents == []


def test_no_trace_writes_nothing(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = SimpleNamespace(
        psychology=SimplePsychologyPolicy(), artifacts=_Keeping(TraceLevel.OFF)
    )
    native.rules("trace").record_psychology_trace(controller, object(), (), world.minute_index)
    assert controller.artifacts.documents == []


def test_a_person_with_no_final_delta_is_left_to_the_python_step(
    generated_scenario: Any, tmp_path: Path
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    artifacts = RunArtifactWriter(generated_scenario, tmp_path, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    view = controller._view_after_movement(world)
    controller.psychology = SimplePsychologyPolicy()
    controller.artifacts = _Keeping(TraceLevel.FULL)
    result = native.rules("psychology").update_psychology(controller, view, 0)
    with pytest.raises(native.Unsupported, match="no final delta"):
        native.rules("trace").record_psychology_trace(controller, result, result.deltas[:-1], 0)
    assert controller.artifacts.documents == []

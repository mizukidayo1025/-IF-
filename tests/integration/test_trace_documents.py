"""The trace documents in Rust (rules/trace.rs), against the Python rules' documents, bit by bit.

A Run with a trace goes through the driver and its kernels. Here each model writes its files on the
driver and with the kernels removed, and random worlds reach values no scenario does. Decision D1 of
docs/rust-port/step4-plan.md: the documents keep their shape. See docs/rust-port/lanes/l6-trace.md.
The psychology, contact and information steps have no Python rule left (lane L10): what the
deleted rules wrote was recorded once as digests (ANSWERS), which the kernels' must equal on Linux.

Model version 2 (step 4, L11: the pinned libm crate and the information events' draw keys)
changed some of these results: those digests were re-recorded from the kernels of that model
and are no longer the deleted Python rules'.
"""

from __future__ import annotations

import hashlib
import json
import os
import random
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from support.paths import BENCHMARKS
from support.python_rules import without_kernels

from sheltersim import native
from sheltersim.core.engine import TraceLevel
from sheltersim.core.kernels import KernelRefusal
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
# The psychology steps' (`_psychology_update` and `_record_psychology_trace` at 45a9bc9, on
# policies/psychology.py and alternatives.py) are in PSYCHOLOGY_ANSWERS.
ANSWERS: dict[str, str] = {
    "information-evidence-aware-rumor-v1-full": (
        "91d8f118da069b5cf6c6e49ae7a3893dde42d9643bcfec74b8026fe2c2e03312"
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
        "91d8f118da069b5cf6c6e49ae7a3893dde42d9643bcfec74b8026fe2c2e03312"
    ),
    "psychology-mean-reverting-life-state-v1-summary": (
        "3364324ebbffb532ae89cdfd667900f3f780cc3c5a08f3041154d178ec2beb32"
    ),
    "social-degroot-contact-v1-full": (
        "d46c4311c3f14eeff494de4075be821334d572a7ea2706d42f6668a716cacb7c"
    ),
    "social-degroot-contact-v1-summary": (
        "170267ecc4338ee8c605fd1ca74b2e405b01e54857384bf286924503348b3d71"
    ),
    "social-expressiveness-channel-openness-v1-full": (
        "91d8f118da069b5cf6c6e49ae7a3893dde42d9643bcfec74b8026fe2c2e03312"
    ),
    "social-sparse-family-zone-contact-v1-full": (
        "a0b6387140df3ebe9cfc641f7792062c9d84b877e1147a8f7261c40ccd99517c"
    ),
}
_RECORD = os.environ.get("SHELTERSIM_RECORD_CONTACT_RUNS")
PSYCHOLOGY_ANSWERS: dict[str, str] = {
    "information-evidence-aware-rumor-v1-full": (
        "67c58f9b0ec9d181559d236c0b5798526509426d7067ad2ae5c94710a9e603dc"
    ),
    "information-hazard-information-v1-full": (
        "cc56c5df0fedfeed07d18da6accf33f3812fe5cb592d806d62a7e5d039221da5"
    ),
    "information-hazard-information-v1-summary": (
        "cc56c5df0fedfeed07d18da6accf33f3812fe5cb592d806d62a7e5d039221da5"
    ),
    "information-sparse-information-contact-v1-full": (
        "1ba1960873ed3bf1d7418ca10c091073c4a32fac857cab7d57c254a2b736eecf"
    ),
    "ints-full": ("abaed98d7e6d7a28a3d53028be0c3c5f084a104613ffbd306a72bdb5c74c627b"),
    "ints-summary": ("abaed98d7e6d7a28a3d53028be0c3c5f084a104613ffbd306a72bdb5c74c627b"),
    "models-0-full": ("640ed4e7a101957606fcddcd486c7030758ff4658be73a39bf25370877272c81"),
    "models-0-summary": ("640ed4e7a101957606fcddcd486c7030758ff4658be73a39bf25370877272c81"),
    "models-1-full": ("cfa05fafeec31e3646ed5207a524a17be226b2eb2f6eab428ffbc628c49ef0e5"),
    "models-1-summary": ("cfa05fafeec31e3646ed5207a524a17be226b2eb2f6eab428ffbc628c49ef0e5"),
    "models-2-full": ("36b75b85d6acd4c719b7c3b8fbf456ac773665de29e0b669f350c6e959a19de4"),
    "models-2-summary": ("36b75b85d6acd4c719b7c3b8fbf456ac773665de29e0b669f350c6e959a19de4"),
    "models-3-full": ("787c2c165087682b368863caab8719d90719eebbfa85df6a49aca1ad25aeac2d"),
    "models-3-summary": ("787c2c165087682b368863caab8719d90719eebbfa85df6a49aca1ad25aeac2d"),
    "models-4-full": ("735c4e5d454fa51f58798bd8fd699f0aabd6c04aee6a2f25153a26f6f0753fc7"),
    "models-4-summary": ("735c4e5d454fa51f58798bd8fd699f0aabd6c04aee6a2f25153a26f6f0753fc7"),
    "models-5-full": ("4d964b10d02eafeabfc15ea50eace84a454a013e51beed194e8eb1a2ecb3338a"),
    "models-5-summary": ("4d964b10d02eafeabfc15ea50eace84a454a013e51beed194e8eb1a2ecb3338a"),
    "psychology-bounded-stress-v1-full": (
        "9c5f529b5e0a9319970986f5155f1cd84ae72a8eea3c889d67765b82582f4a7e"
    ),
    "psychology-causal-stress-v1-full": (
        "c9fc0b3623eb82283792189dc457c8bb5cbb2827f48658dc409d76aa0d2f99d3"
    ),
    "psychology-causal-stress-v1-summary": (
        "c9fc0b3623eb82283792189dc457c8bb5cbb2827f48658dc409d76aa0d2f99d3"
    ),
    "psychology-life-state-feedback-v1-full": (
        "1c4cce80a6f63956dc0496fcec4c532cd57330cab33f0b659b1fdcc188e89de0"
    ),
    "psychology-mean-reverting-distress-v1-full": (
        "1ced8a5fd869b4f02716b6249857e89094293e4cdf5f79d38c983771286a0d36"
    ),
    "psychology-mean-reverting-life-state-v1-full": (
        "67c58f9b0ec9d181559d236c0b5798526509426d7067ad2ae5c94710a9e603dc"
    ),
    "psychology-mean-reverting-life-state-v1-summary": (
        "67c58f9b0ec9d181559d236c0b5798526509426d7067ad2ae5c94710a9e603dc"
    ),
    "social-degroot-contact-v1-full": (
        "8c3eeec771be56bebfd68ad2cc5ccc31fff88477d7a84d717f156018166d01d6"
    ),
    "social-degroot-contact-v1-summary": (
        "8c3eeec771be56bebfd68ad2cc5ccc31fff88477d7a84d717f156018166d01d6"
    ),
    "social-expressiveness-channel-openness-v1-full": (
        "67c58f9b0ec9d181559d236c0b5798526509426d7067ad2ae5c94710a9e603dc"
    ),
    "social-sparse-family-zone-contact-v1-full": (
        "41b28283442422e6792230377e316ec955e7429df6c2ce1d2e8f42d914728c5b"
    ),
}


def _answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    if _RECORD:
        with open(_RECORD, "a", encoding="utf-8") as file:
            file.write(json.dumps({name: digest.hexdigest()}) + "\n")
        return
    assert digest.hexdigest() == ANSWERS[name], name


def _psychology_answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    assert digest.hexdigest() == PSYCHOLOGY_ANSWERS[name], name


def _python_contact_steps(patch: pytest.MonkeyPatch) -> None:
    """Only while recording: the deleted Python steps of the contact models."""
    from sheltersim.experiments.phases.sample import SamplePhase
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
    """The decision file against the Python rules of a Run without the kernels; the psychology
    and contact files, whose Python steps are gone, against what those steps wrote."""
    scenario = _model_scenario(domain, model)
    before = _ticks()
    with pytest.MonkeyPatch.context() as patch:
        if _RECORD:
            _python_contact_steps(patch)
        run_scenario(scenario, tmp_path / "driver", trace_level=level)
    if not _RECORD:
        assert _ticks() - before == scenario.duration_minutes
    driven = _files(tmp_path / "driver")
    view_step = OfflineSimulationController.__dict__["_view_after_movement"]
    with pytest.MonkeyPatch.context() as patch:
        without_kernels(patch)
        # The psychology trace, which has no Python step, reads a view the kernel made.
        patch.setattr(OfflineSimulationController, "_view_after_movement", view_step)
        run_scenario(scenario, tmp_path / "python", trace_level=level)
    assert driven["psychology_contributions.jsonl"]
    contacts = ("social_contacts.jsonl", "information_contacts.jsonl")
    psychology = ("psychology_contributions.jsonl", "psychology_contributions_hourly.jsonl")
    python = _files(tmp_path / "python")
    for name, raw in python.items():
        if name not in contacts:
            assert driven[name] == raw, name
    _psychology_answers(
        f"{domain}-{model}-{level.value}",
        [(driven[name] or b"").decode("utf-8") for name in psychology],
    )
    _answers(
        f"{domain}-{model}-{level.value}",
        [(driven[name] or b"").decode("utf-8") for name in contacts],
    )


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


def _documents(controller: Any, view: Any, final_of: Any) -> str:
    """The step's document of the minute and its contributions, as text."""
    result = native.rules("psychology").update_psychology(controller, view, 0)
    native.rules("trace").record_psychology_trace(controller, result, final_of(result.deltas), 0)
    (document,) = controller.artifacts.documents
    return document


@pytest.mark.parametrize("seed", range(6))
@pytest.mark.parametrize(
    "level", [TraceLevel.SUMMARY, TraceLevel.FULL], ids=lambda level: level.value
)
def test_the_document_of_each_model_is_the_deleted_rule_s(
    generated_scenario: Any, tmp_path: Path, seed: int, level: TraceLevel
) -> None:
    draws = random.Random(seed)
    world = WorldState.from_scenario(generated_scenario)
    artifacts = RunArtifactWriter(generated_scenario, tmp_path, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    _shuffle(world, draws)
    view = controller._view_after_movement(world)
    documents = []
    for policy in _policies(draws, sorted(world.agents)):
        controller.psychology = policy
        controller.artifacts = _Keeping(level)
        # What the social step does between the two.
        documents.append(
            _documents(
                controller,
                view,
                lambda deltas: tuple(
                    delta.with_stress(draws.random()) if number % 2 else delta
                    for number, delta in enumerate(deltas)
                ),
            )
        )
    _psychology_answers(f"models-{seed}-{level.value}", documents)


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
def test_int_values_of_a_scenario_give_the_deleted_rule_s_document(
    generated_scenario: Any, tmp_path: Path, level: TraceLevel
) -> None:
    """A separated household's pressure is the int `discontent_stress_rate`: the Python rule kept
    it an int in `life_state_before` and `after`, and JSON writes `1`, not `1.0`."""
    world = WorldState.from_scenario(generated_scenario)
    artifacts = RunArtifactWriter(generated_scenario, tmp_path, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    _shuffle(world, random.Random(3))
    view = controller._view_after_movement(world)
    separated = 0
    documents = []
    for policy in _int_policies(sorted(world.agents)):
        controller.psychology = policy
        controller.artifacts = _Keeping(level)
        document = _documents(controller, view, lambda deltas: deltas)
        documents.append(document)
        separated += document.count(
            '"family_separation_pressure": {"before": 1, "after": 1, "delta": 0}'
        )
    assert separated > 0
    _psychology_answers(f"ints-{level.value}", documents)


def test_traces_that_are_not_the_kernel_s_rows_are_refused(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = SimpleNamespace(
        psychology=SimplePsychologyPolicy(), artifacts=_Keeping(TraceLevel.SUMMARY)
    )
    result = SimpleNamespace(traces=(), deltas=())
    with pytest.raises(KernelRefusal, match="not the psychology kernel's rows"):
        OfflineSimulationController._record_psychology_trace(
            controller, result, (), world.minute_index
        )
    assert controller.artifacts.documents == []


def test_no_trace_writes_nothing(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = SimpleNamespace(
        psychology=SimplePsychologyPolicy(), artifacts=_Keeping(TraceLevel.OFF)
    )
    native.rules("trace").record_psychology_trace(controller, object(), (), world.minute_index)
    assert controller.artifacts.documents == []


def test_a_person_with_no_final_delta_is_refused(generated_scenario: Any, tmp_path: Path) -> None:
    world = WorldState.from_scenario(generated_scenario)
    artifacts = RunArtifactWriter(generated_scenario, tmp_path, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    view = controller._view_after_movement(world)
    controller.psychology = SimplePsychologyPolicy()
    controller.artifacts = _Keeping(TraceLevel.FULL)
    result = native.rules("psychology").update_psychology(controller, view, 0)
    with pytest.raises(KernelRefusal, match="no final delta"):
        OfflineSimulationController._record_psychology_trace(
            controller, result, result.deltas[:-1], 0
        )
    assert controller.artifacts.documents == []

"""The trace documents in Rust (rules/trace.rs), against what the Python rules wrote, bit by bit.

A Run with a trace goes through the driver and its kernels. Here each model writes its files on the
driver, and random worlds reach values no scenario does. Decision D1 of
docs/rust-port/step4-plan.md: the documents keep their shape. See docs/rust-port/lanes/l6-trace.md.
The psychology, contact and information steps have no Python rule left (lane L10): what the
deleted rules wrote was recorded once as digests (ANSWERS), which the kernels' must equal on every
OS. The decision file was compared with a second Run on the Python validators until #118; since
then it is held to the digests of what both Runs wrote (DECISION_ANSWERS).

Model version 2 (step 4, L11: the pinned libm crate and the information events' draw keys)
changed some of these results: those digests were re-recorded from the kernels of that model
and are no longer the deleted Python rules'.

The documents of each model on the shuffled worlds are the core's test
(`the_document_of_each_model_is_the_deleted_rule_s` of `native/core/tests/psychology_steps.rs`,
#225), with the same digests; this file keeps the entry test of the ints a Scenario may write.
"""

from __future__ import annotations

import hashlib
import json
import random
import sys
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from support.paths import BENCHMARKS

from sheltersim import native
from sheltersim.core.engine import TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.models import SupportRequestState, SupportRequestStatus
from sheltersim.experiments.runner import OfflineSimulationController, run_scenario
from sheltersim.inspection.traces import RunArtifactWriter
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
# sha256 of the files, recorded once on Linux.
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


# The sha256 of decisions.jsonl. Until #118 a second Run with the kernels' checks replaced by the
# Python validators (support/python_rules.py without_kernels) had to write the same bytes; these
# are the bytes both wrote, recorded on Linux at 77eef389.
DECISION_ANSWERS: dict[str, str] = {
    "information-evidence-aware-rumor-v1-full": (
        "dbef166be2fa306e5079556406a998826cf83836c9a59dc02d4ce3d4aeb8ef54"
    ),
    "information-hazard-information-v1-full": (
        "6e11f5681ea5b7d805c4b7385845f062c0d864d7536b8067673d25603c809e2b"
    ),
    "information-hazard-information-v1-summary": (
        "1557169dd42caaadcc890596750aa9e19a7d51f2e9ab9fd53e988caed9509185"
    ),
    "information-sparse-information-contact-v1-full": (
        "62094143ed697e09ee30ebbddbe969b59a82329752631aa8619a8002f4d7e170"
    ),
    "psychology-bounded-stress-v1-full": (
        "0361f25c5ed030e0411a9b5cc33c7b9c41ac7455dc6813608f6a296f302677c0"
    ),
    "psychology-causal-stress-v1-full": (
        "c45e87cdd119775d0288e2446e7f14c0d710806b362b5c1614e24171ac48c0c0"
    ),
    "psychology-causal-stress-v1-summary": (
        "ca42dbf6929359c52bb8a3a63be2d5bb1ba0ff491d6ad4ac36f46479cb15e2ef"
    ),
    "psychology-life-state-feedback-v1-full": (
        "be07f9a11db1f210f1b4fc39be95a9c68628d11d40853ed262473286724b033e"
    ),
    "psychology-mean-reverting-distress-v1-full": (
        "0e64cfe9a860c1d043849a59fa40362f9724c974b31814f4acfd0bbf7d8c1765"
    ),
    "psychology-mean-reverting-life-state-v1-full": (
        "dbef166be2fa306e5079556406a998826cf83836c9a59dc02d4ce3d4aeb8ef54"
    ),
    "psychology-mean-reverting-life-state-v1-summary": (
        "4a9a4f42b9abd9588caa7cf5988b83b9a31cc3940d58c4f579bb69311b4016a5"
    ),
    "social-degroot-contact-v1-full": (
        "9ad47625159fd43d463eb05e2902684f0693d8b6619e318c91b2374ceba45ade"
    ),
    "social-degroot-contact-v1-summary": (
        "3bce01320aa79219e75f372c0aebb65c1e52fa2121906f13cd62cb450057a74b"
    ),
    "social-expressiveness-channel-openness-v1-full": (
        "dbef166be2fa306e5079556406a998826cf83836c9a59dc02d4ce3d4aeb8ef54"
    ),
    "social-sparse-family-zone-contact-v1-full": (
        "790197b97362f20f5b5f07badf4a668ec6a8af4b71d6e6427b3d228935ee997d"
    ),
}


def _answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    assert digest.hexdigest() == ANSWERS[name], name


def _psychology_answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    assert digest.hexdigest() == PSYCHOLOGY_ANSWERS[name], name


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
def test_a_run_with_a_trace_writes_the_recorded_bytes(
    domain: str, model: str, level: TraceLevel, tmp_path: Path
) -> None:
    """The decision file against what a Run on the Python validators wrote; the psychology and
    contact files, whose Python steps are gone, against what those steps wrote."""
    scenario = _model_scenario(domain, model)
    before = _ticks()
    run_scenario(scenario, tmp_path / "driver", trace_level=level)
    assert _ticks() - before == scenario.duration_minutes
    driven = _files(tmp_path / "driver")
    assert driven["psychology_contributions.jsonl"]
    name = f"{domain}-{model}-{level.value}"
    decisions = driven["decisions.jsonl"]
    assert decisions is not None
    assert hashlib.sha256(decisions).hexdigest() == DECISION_ANSWERS[name], name
    contacts = ("social_contacts.jsonl", "information_contacts.jsonl")
    psychology = ("psychology_contributions.jsonl", "psychology_contributions_hourly.jsonl")
    _psychology_answers(name, [(driven[file] or b"").decode("utf-8") for file in psychology])
    _answers(name, [(driven[file] or b"").decode("utf-8") for file in contacts])


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
        # The kernels read the policies the Run read (lane P8 a3).
        controller._run_core.refresh_policies(controller)
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
    controller._run_core.refresh_policies(controller)
    result = native.rules("psychology").update_psychology(controller, view, 0)
    with pytest.raises(KernelRefusal, match="no final delta"):
        OfflineSimulationController._record_psychology_trace(
            controller, result, result.deltas[:-1], 0
        )
    assert controller.artifacts.documents == []

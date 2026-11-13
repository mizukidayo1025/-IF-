"""Contact-based stress propagation runs in a full Run, survives a Checkpoint and
writes a compact contact trace only when tracing is on.

The first test keeps the earlier sparse policy (WorldState schema 5) on
scenario_100_agents; the second runs the shared generated Scenario, whose social
policy (expressiveness-channel-openness-v1) is the one an operator's Scenario uses
(#137).

The policy itself is checked in tests/unit/policies/test_social.py.
"""

from __future__ import annotations

import copy
import hashlib
import json
from pathlib import Path

import pytest
from support.paths import SCENARIO_100_AGENTS

from sheltersim.core.checkpoint import Checkpoint
from sheltersim.core.engine import TraceLevel
from sheltersim.experiments.runner import run_scenario
from sheltersim.experiments.scenario import ScenarioSpec

# What the deleted Python steps wrote (`_social_propagation` and `_sample_information_contacts` of
# experiments/phases at 2d8fb46, on policies/social.py, information.py and alternatives.py), as
# sha256 of the files, recorded once on Linux.
ANSWERS: dict[str, str] = {
    "no-weight-channel_strength": (
        "3041fc74e645c94901b30039a824bc8e94647b88ab982180eaf9f53b550d936e"
    ),
    "no-weight-source_expressiveness": (
        "35873748a603027ebb4d61c423661888d4dadda863c7d944b5745fd0bb5b9c11"
    ),
    "no-weight-stress_sensitivity": (
        "6b4b0799f0f3b3fa469ac872422e88e1d9a3a42feaaffc1a17fcd8854c5d4e9e"
    ),
}


def _answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    assert digest.hexdigest() == ANSWERS[name], name


def test_enabled_policy_is_checkpointable_and_writes_compact_contact_trace(
    tmp_path: Path,
) -> None:
    document = json.loads(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    document["duration_minutes"] = 3
    document["scheduled_events"] = []
    document["profiles"] = document["profiles"][:2]
    document["model_configs"]["social_propagation"] = {
        "enabled": True,
        "max_contacts_per_agent": 1,
        "minimum_contact_minutes": 1,
        "stress_spread_per_minute": 0.01,
        "natural_recovery_per_minute": 0.001,
        "cooldown_minutes": 5,
    }
    scenario = ScenarioSpec.model_validate(document)

    world = run_scenario(scenario, tmp_path / "social-run")
    untraced_world = run_scenario(
        scenario,
        tmp_path / "social-run-off-trace",
        trace_level=TraceLevel.OFF,
    )

    assert world.schema_version == 5
    assert world.canonical_json() == untraced_world.canonical_json()
    assert world.model_state["social_propagation_v1"]["schema_version"] == 1
    restored = Checkpoint.capture(world).restore_world()
    assert restored.model_state == world.model_state
    assert restored.canonical_json() == world.canonical_json()
    manifest = json.loads((tmp_path / "social-run" / "manifest.json").read_text(encoding="utf-8"))
    assert (
        manifest["model_assumptions"]["social_propagation"]["source_id"]
        == "ASM-SOCIAL-PROPAGATION-V1"
    )
    traces = [
        json.loads(line)
        for line in (tmp_path / "social-run" / "social_contacts.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    assert len(traces) == 3
    assert all(item["selected_contact_count"] <= 2 for item in traces)
    assert all(len(item["contacts"]) <= 20 for item in traces)
    assert (tmp_path / "social-run-off-trace" / "social_contacts.jsonl").read_text(
        encoding="utf-8"
    ) == ""


# Contacts count only after minimum_contact_minutes (5) together, so the first ones
# appear at minute 4; fifteen minutes gives several minutes with contacts.
GENERATED_MINUTES = 15


def test_generated_scenario_propagation_is_checkpointable_and_writes_compact_contact_trace(
    tmp_path: Path, generated_previews
) -> None:
    document = copy.deepcopy(generated_previews()["scenario"])
    document["duration_minutes"] = GENERATED_MINUTES
    scenario = ScenarioSpec.model_validate(document)
    config = scenario.model_configs["social_propagation"]
    assert config["enabled"] is True

    world = run_scenario(scenario, tmp_path / "social-run")
    untraced_world = run_scenario(
        scenario,
        tmp_path / "social-run-off-trace",
        trace_level=TraceLevel.OFF,
    )

    assert world.canonical_json() == untraced_world.canonical_json()
    restored = Checkpoint.capture(world).restore_world()
    assert restored.model_state == world.model_state
    assert restored.canonical_json() == world.canonical_json()
    manifest = json.loads((tmp_path / "social-run" / "manifest.json").read_text(encoding="utf-8"))
    assert manifest["model_assumptions"]["social_propagation"]["source_id"] == config["source_id"]
    traces = [
        json.loads(line)
        for line in (tmp_path / "social-run" / "social_contacts.jsonl")
        .read_text(encoding="utf-8")
        .splitlines()
    ]
    assert len(traces) == GENERATED_MINUTES
    # Otherwise the checks below would pass on an empty trace.
    assert any(item["selected_contact_count"] for item in traces)
    assert all(
        item["selected_contact_count"] <= config["max_contacts_per_agent"] * len(world.agents)
        for item in traces
    )
    assert all(len(item["contacts"]) <= 20 for item in traces)
    assert (tmp_path / "social-run-off-trace" / "social_contacts.jsonl").read_text(
        encoding="utf-8"
    ) == ""


@pytest.mark.parametrize(
    "case", ["stress_sensitivity", "source_expressiveness", "channel_strength"]
)
def test_generated_scenario_without_contact_weight_runs_without_propagation(
    tmp_path: Path, generated_previews, case: str
) -> None:
    """#87 (元 #301): every contact weight is 0, so no stress moves, and the Run goes on.

    Each case sets one factor of the weight (expressiveness × channel strength ×
    stress_sensitivity) to 0 through valid Scenario values. Before, the Run stopped with the
    ValueError of the Python rule's strict zip, and the Rust kernel handed the tick to it.
    The kernel writes the world and the contact trace the Python rules wrote.
    """
    document = copy.deepcopy(generated_previews()["scenario"])
    document["duration_minutes"] = GENERATED_MINUTES
    social = document["model_configs"]["social_propagation"]
    assert social["source_id"] == "ASM-BOSSE-AFFECT-CONTAGION-V1"
    if case == "stress_sensitivity":
        for profile in document["profiles"]:
            profile["stress_sensitivity"] = 0.0
    elif case == "source_expressiveness":
        social["source_expressiveness_by_agent"] = {
            profile["id"]: 0.0 for profile in document["profiles"]
        }
    else:
        for basis in ("family", "same_zone", "nearby_zone"):
            social[f"{basis}_channel_strength"] = 0.0
    scenario = ScenarioSpec.model_validate(document)

    world = run_scenario(scenario, tmp_path / "run")
    written = (tmp_path / "run" / "social_contacts.jsonl").read_text(encoding="utf-8")
    _answers(f"no-weight-{case}", [world.canonical_json(), written])
    traces = [json.loads(line) for line in written.splitlines()]
    assert len(traces) == GENERATED_MINUTES
    assert any(item["selected_contact_count"] for item in traces)
    assert all(item["contributing_contact_count"] == 0 for item in traces)
    assert all(item["total_stress_added"] == 0.0 for item in traces)
    assert all(item["contributions"] == [] for item in traces)

"""benchmarks/first_difference.py and benchmarks/tick_probe.py: the tools that say where two
runs first differ, the values behind random comparisons, and the reversed-order variant.

The rules for using them are in docs/testing.md.
"""

from __future__ import annotations

import copy
import json
import os
import shutil
import subprocess
import sys
from pathlib import Path

import pytest
from support.paths import BENCHMARKS, GATE_ALL_PATHS

from sheltersim.core.engine import TickEngine, TickPhase
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.inspection.traces import RunArtifactWriter

sys.path.insert(0, str(BENCHMARKS))
import first_difference  # noqa: E402
import tick_probe  # noqa: E402

TOOL = BENCHMARKS / "first_difference.py"
SOURCE = BENCHMARKS.parent / "src"
TESTS = BENCHMARKS.parent / "tests"
# Put on PYTHONPATH, it runs the recorded process on the Python rules, whose draws the recorder logs
# and which _patched_source can edit: the `python_rules` mark. (The contact handler's Python rule,
# whose correction draw a second case edited, and the information rule, which the other cases
# edited, are gone: lane L10. A bug in a step only Rust runs is put in by a patch of the step.)
PYTHON_RULES_SITE = f"""\
import sys

import pytest

sys.path.insert(0, {str(TESTS)!r})
from support.python_rules import without_kernels

without_kernels(pytest.MonkeyPatch())
"""
# Rumors, corrections and the information model, in a few seconds.
RUMORS = "model__information__evidence-aware-rumor-v1"
# The behavior rule's choice, a draw against the cumulative probabilities, in Python under the mark.
CHOICE = "            cumulative += candidate.probability\n"
# A bug a lane might make in the information step: a bigger decay of the query pressure, put after
# the kernel's step since only Rust has the rule.
DECAY_BUG = """
from sheltersim.domain.information import INFORMATION_SPREAD_STATE_KEY
from sheltersim.experiments.runner import OfflineSimulationController

_step = OfflineSimulationController._sample_information_contacts


def _with_a_bug(self, minute, rng):
    _step(self, minute, rng)
    pressure = self.world.model_state[INFORMATION_SPREAD_STATE_KEY]["query_pressure"]
    for agent_id in pressure:
        pressure[agent_id] = max(0.0, pressure[agent_id] - 0.001)


OfflineSimulationController._sample_information_contacts = _with_a_bug
"""
LINUX_ONLY = pytest.mark.skipif(sys.platform != "linux", reason="hashes are pinned on Linux")


def _tool(*arguments: object, paths: tuple[Path, ...] = ()) -> subprocess.CompletedProcess[str]:
    env = {**os.environ, "SHELTERSIM_LLM_MODE": "offline"}
    if paths:
        env["PYTHONPATH"] = os.pathsep.join(
            [*map(str, paths), *filter(None, os.environ.get("PYTHONPATH", "").split(os.pathsep))]
        )
    return subprocess.run(
        [sys.executable, str(TOOL), *map(str, arguments)],
        env=env,
        cwd=BENCHMARKS.parent,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )


def _patched_source(directory: Path, relative: str, before: str, after: str) -> Path:
    """A copy of src/ with one edit: the bug a lane might make, never in the repository."""
    root = directory / "src"
    shutil.copytree(SOURCE, root, ignore=shutil.ignore_patterns("__pycache__"))
    target = root / relative
    text = target.read_text(encoding="utf-8")
    assert text.count(before) == 1
    target.write_text(text.replace(before, after), encoding="utf-8")
    return root


def _recorded(output: Path, *arguments: object, path: Path | None = None, patch: str = "") -> Path:
    """A stream of the Python rules, which are what _patched_source edits: Rust moved the rest.
    `patch` is more of the site's code, run after the mark's change."""
    site = output.parent / f"python_rules_{output.name}"
    site.mkdir(parents=True, exist_ok=True)
    (site / "sitecustomize.py").write_text(PYTHON_RULES_SITE + patch, encoding="utf-8")
    paths = (site,) if path is None else (site, path)
    done = _tool("--record", RUMORS, "--output", output, "--draws", *arguments, paths=paths)
    assert done.returncode == 0, done.stdout + done.stderr
    return output


@pytest.fixture(scope="module")
def base_stream(tmp_path_factory: pytest.TempPathFactory) -> Path:
    return _recorded(tmp_path_factory.mktemp("base") / "stream")


@LINUX_ONLY
def test_recording_leaves_the_expected_hashes(base_stream: Path) -> None:
    """Digests and observed draws are invisible to the run."""
    result = json.loads((base_stream / "result.json").read_text(encoding="utf-8"))
    expected = json.loads(GATE_ALL_PATHS.read_text(encoding="utf-8"))["scenarios"][RUMORS]
    assert result["runs"] == expected["runs"]


def test_two_recordings_of_a_scenario_compare_as_the_same(
    base_stream: Path, tmp_path: Path
) -> None:
    done = _tool(base_stream, _recorded(tmp_path / "again"))

    assert done.returncode == 0, done.stdout + done.stderr
    assert "digests: the same" in done.stdout
    assert "final hashes: the same" in done.stdout


def test_an_injected_bug_is_named_by_minute_phase_part_path_and_hex(
    base_stream: Path, tmp_path: Path
) -> None:
    changed = _recorded(tmp_path / "changed", patch=DECAY_BUG)

    done = _tool(base_stream, changed)

    assert done.returncode == 1, done.stdout + done.stderr
    assert "phase sample_endogenous_events" in done.stdout
    assert "model_state/information_spread_v1" in done.stdout
    assert "model_state/information_spread_v1.query_pressure.A" in done.stdout
    assert "(float 0x" in done.stdout
    assert "final hashes: differ" in done.stdout


@pytest.mark.parametrize(
    ("label", "relative", "before", "after"),
    [
        (
            "behavior",
            "sheltersim/policies/behavior.py",
            CHOICE,
            CHOICE.replace("candidate.probability", "candidate.probability * (1 + 2**-40)"),
        ),
    ],
    ids=["choice_plus_two_to_the_minus_forty"],
)
def test_a_small_change_of_a_probability_is_caught_though_no_comparison_flips(
    base_stream: Path, tmp_path: Path, label: str, relative: str, before: str, after: str
) -> None:
    """The gate's weak cases (docs/rust-port.md): the world and the hashes stay the same."""
    source = _patched_source(tmp_path, relative, before, after)
    changed = _recorded(tmp_path / "changed", path=source)

    done = _tool(base_stream, changed)

    assert done.returncode == 1, done.stdout + done.stderr
    assert "digests: the same" in done.stdout
    assert "final hashes: the same" in done.stdout
    original = tick_probe.uses_of_draws(base_stream / "draws.jsonl", label)
    patched = tick_probe.uses_of_draws(changed / "draws.jsonl", label)
    assert original and [row[0] for row in original] == [row[0] for row in patched]
    assert [row[1] for row in original] != [row[1] for row in patched]


def test_a_run_that_stops_early_is_reported_where_its_stream_ends(
    base_stream: Path, tmp_path: Path
) -> None:
    shorter = tmp_path / "shorter"
    shorter.mkdir()
    lines = (base_stream / "digests.jsonl").read_text(encoding="utf-8").splitlines(keepends=True)
    (shorter / "digests.jsonl").write_text("".join(lines[:5]), encoding="utf-8")

    done = _tool(shorter, base_stream, "--no-values")

    assert done.returncode == 1
    assert "stream A ends here" in done.stdout


def test_the_differences_give_paths_by_id_and_tell_ints_from_floats() -> None:
    first = {"agents": [{"profile": {"id": "A"}, "stress": 0.5}], "n": 1, "only": True}
    second = {"agents": [{"profile": {"id": "A"}, "stress": 0.25}], "n": 1.0}

    found = list(first_difference.differences(first, second, "world"))

    assert [row[0] for row in found] == ["world.agents[id=A].stress", "world.n", "world.only"]
    assert first_difference.value_text(0.5) == "0.5 (float 0x1.0000000000000p-1)"
    assert first_difference.value_text(1) == "1 (int)"


def test_an_edit_of_old_history_shows_when_it_is_hashed_from_the_first_item(
    generated_scenario, tmp_path: Path
) -> None:
    """History items are hashed once per stream, and again every so many minutes."""
    world = WorldState.from_scenario(generated_scenario)
    world.model_state["body_state_v1"] = {"history": [{"n": 1}, {"n": 2}]}
    # Read back: an owned namespace stores a tracked tree, not the list it was given.
    history = world.model_state["body_state_v1"]["history"]
    path = tmp_path / "digests.jsonl"
    stream = tick_probe.DigestStream(path, full_every_minutes=2)
    stream.engine_started(0, world)
    stream.after_phase(0, world, 1, TickPhase.COLLECT_METRICS)
    history[0]["n"] = 9
    stream.after_phase(0, world, 1, TickPhase.COLLECT_METRICS)
    stream.after_phase(0, world, 2, TickPhase.COLLECT_METRICS)
    history.append({"n": 3})
    stream.after_phase(0, world, 3, TickPhase.COLLECT_METRICS)
    stream.close()

    rows = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]
    part = "model_state/body_state_v1"
    assert [part in row["d"] for row in rows] == [True, False, False, True, True]


# Observed draws --------------------------------------------------------------------------


def test_a_draw_logs_each_comparison_either_way_and_stays_a_float() -> None:
    uses: list[list[object]] = []
    draw = tick_probe.ObservedDraw(0.25, uses)

    assert draw < 0.5
    assert 0.5 > draw
    assert not draw >= 0.5
    assert int(draw * 8) == 2
    assert json.dumps({"x": draw}) == '{"x": 0.25}'
    assert type(copy.deepcopy(draw)) is float

    half = (0.5).hex()
    assert uses == [
        ["lt", half, True],
        ["lt", half, True],
        ["ge", half, False],
        ["mul", 8],
    ]


def test_the_recorder_gives_the_same_draws_and_undoes_its_patch(tmp_path: Path) -> None:
    plain = CounterRNG.random
    expected = CounterRNG(7).random("label", 1, "A")
    recorder = tick_probe.DrawRecorder(tmp_path / "draws.jsonl")
    # A type check of the kernels' callers must still pass.
    rng = CounterRNG(7)
    assert type(rng) is CounterRNG
    assert rng.random("label", 1, "A") < 2.0
    assert float(CounterRNG(7).random("label", 1, "A")) == expected
    recorder.close()

    assert CounterRNG.random is plain
    rows = [
        json.loads(line)
        for line in (tmp_path / "draws.jsonl").read_text(encoding="utf-8").splitlines()
    ]
    assert rows[0]["k"] == ["label", 1, "A"]
    assert rows[0]["d"] == expected.hex()
    assert rows[0]["u"] == [["lt", (2.0).hex(), True]]


# Order -----------------------------------------------------------------------------------


def _engine(scenario: object, directory: Path) -> TickEngine:
    world = WorldState.from_scenario(scenario)
    artifacts = RunArtifactWriter(scenario, directory)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    return TickEngine(world, phase_handler=controller)


def test_reversing_keeps_every_object_and_value_and_reverses_each_dict(generated_scenario) -> None:
    world = WorldState.from_scenario(generated_scenario)
    inner = {"a": 1, "b": {"c": 2, "d": 3}}
    world.model_state["probe"] = {"x": inner, "y": [{"e": 4, "f": 5}]}
    agents, order = world.agents, list(world.agents)
    before = copy.deepcopy(world.model_state)

    tick_probe.reverse_order(world)

    assert world.agents is agents and list(agents) == order[::-1]
    assert world.model_state["probe"]["x"] is inner
    assert world.model_state == before
    assert list(inner) == ["b", "a"]
    assert list(inner["b"]) == ["d", "c"]
    assert list(world.model_state["probe"]["y"][0]) == ["f", "e"]


def test_the_reversed_order_shows_a_rule_that_reads_the_first_agent(
    generated_scenario, tmp_path: Path
) -> None:
    seen: list[str] = []

    def rule(phase, world, minute, rng) -> None:
        if phase == "update_agent_state":
            seen.append(next(iter(world.agents)))

    probe = tick_probe.ReversedOrder()
    undo = tick_probe.install([probe])
    try:
        TickEngine(WorldState.from_scenario(generated_scenario), phase_handler=rule).run(2)
    finally:
        undo()

    assert seen[0] != seen[1]
    assert probe.reversals == 14


def test_the_order_changes_inside_phases_at_each_named_place(
    generated_scenario, tmp_path: Path
) -> None:
    """A rule that moves must take its place in tick_probe.WITHIN_PHASE along."""
    probe = tick_probe.ReversedOrder(within_phase=True)
    undo = tick_probe.install([probe])
    try:
        _engine(generated_scenario, tmp_path / "run").run(10)
    finally:
        undo()
        probe.close()

    assert {
        "EventDispatcher.dispatch_one",
        "OfflineSimulationController._run_dispatch_phase",
    } & set(probe.fired)
    assert {
        "ProposePhase._apply_proposal",
        "ToiletServiceManager.arbitrate",
        "StaffService.assign_pending",
    } <= set(probe.fired)
    assert {"WorldView.from_world", "OfflineSimulationController._view_after_movement"} & set(
        probe.fired
    )
    assert probe.reversals > 70


def test_the_gate_flag_compares_the_reversed_orders_with_the_base_run(tmp_path: Path) -> None:
    done = subprocess.run(
        [
            sys.executable,
            str(BENCHMARKS / "gate_all_paths.py"),
            "--output",
            tmp_path / "run",
            "--only",
            "branch_commands",
            "--reversed-order",
        ],
        env={**os.environ, "SHELTERSIM_LLM_MODE": "offline"},
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )

    assert done.returncode == 0, done.stdout + done.stderr
    summary = json.loads((tmp_path / "run" / "summary.json").read_text(encoding="utf-8"))
    assert summary["determinism"]["branch_commands"] == {
        "reversed_order": "same",
        "reversed_within_phase": "same",
    }

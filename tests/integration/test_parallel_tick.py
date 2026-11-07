"""The per-person compute of a tick on several threads gives the bytes of one thread (lane L12).

native/src/core/parallel.rs computes each person's social contacts and entries of the view hash on
any thread and applies the results in the people's order. A compute of few
people stays on the calling thread, so these tests send every person to the helpers, one at a time
(`set_people_per_helper(1)`), and compare the world, its event log and the Run's files, traces
included, with the Run on the calling thread alone.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

# More people than a run of the helpers takes, with the information and social models on.
PEOPLE = 60
DAYS = 0.25

_RUN = """
import hashlib, json, sys
from pathlib import Path
import sheltersim_native
from sheltersim.core.engine import TraceLevel
from sheltersim.experiments.runner import run_scenario
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.storage.projects import digest

scenario_file, output, per_helper = Path(sys.argv[1]), Path(sys.argv[2]), int(sys.argv[3])
sheltersim_native.set_people_per_helper(per_helper)
world = run_scenario(ScenarioSpec.load(scenario_file), output / "run", trace_level=TraceLevel.FULL)
document = world.to_dict()
files = {
    str(path.relative_to(output)): hashlib.sha256(path.read_bytes()).hexdigest()
    for path in sorted((output / "run").rglob("*"))
    if path.is_file()
}
print(json.dumps({
    "helper_threads": sheltersim_native.parallel_state(PEOPLE)[0],
    "helpers": sheltersim_native.parallel_state(PEOPLE)[1],
    "world": digest(document),
    "events": digest(document["event_log"]),
    "files": files,
}))
""".replace("PEOPLE", str(PEOPLE))


def _run(scenario_file: Path, output: Path, per_helper: int, threads: str) -> dict[str, Any]:
    """The Run in a process of its own, with rayon's pool at `threads` threads."""
    environment = {
        **os.environ,
        "SHELTERSIM_LLM_MODE": "offline",
        "RAYON_NUM_THREADS": threads,
        "PYTHONPATH": os.pathsep.join(sys.path),
    }
    done = subprocess.run(
        [sys.executable, "-c", _RUN, str(scenario_file), str(output), str(per_helper)],
        env=environment,
        capture_output=True,
        text=True,
        encoding="utf-8",
        check=False,
    )
    assert done.returncode == 0, done.stdout[-4000:] + done.stderr[-4000:]
    return json.loads(done.stdout.splitlines()[-1])


@pytest.fixture(scope="module")
def scenario_file(generated_previews: Any, tmp_path_factory: pytest.TempPathFactory) -> Path:
    from sheltersim.experiments.scenario import ScenarioSpec

    preview = generated_previews(evacuees=PEOPLE, duration_days=DAYS)
    scenario = ScenarioSpec.model_validate(preview["scenario"])
    configs = scenario.model_configs
    assert configs["information_spread"]["enabled"] and configs["social_propagation"]["enabled"]
    path = tmp_path_factory.mktemp("parallel-tick") / "scenario.json"
    path.write_text(scenario.canonical_json() + "\n", encoding="utf-8")
    return path


def test_four_threads_compute_the_bytes_of_one(scenario_file: Path, tmp_path: Path) -> None:
    one = _run(scenario_file, tmp_path / "one", 1, "1")
    four = _run(scenario_file, tmp_path / "four", 1, "4")

    # One thread has no helper; four are the calling thread and three helpers, which all take part.
    assert (one["helper_threads"], one["helpers"]) == (0, 0)
    assert (four["helper_threads"], four["helpers"]) == (3, 3)
    assert four["world"] == one["world"]
    assert four["events"] == one["events"]
    assert four["files"] == one["files"] and four["files"]


def test_helpers_give_the_bytes_of_the_calling_thread_alone(
    scenario_file: Path, tmp_path: Path
) -> None:
    """In one pool of four threads: no helper (the default for so few people), and every helper."""
    alone = _run(scenario_file, tmp_path / "alone", 10**9, "4")
    helped = _run(scenario_file, tmp_path / "helped", 1, "4")

    assert alone["helpers"] == 0 and helped["helpers"] == 3
    assert (helped["world"], helped["events"], helped["files"]) == (
        alone["world"],
        alone["events"],
        alone["files"],
    )


def test_few_people_stay_on_the_calling_thread() -> None:
    import sheltersim_native

    before = sheltersim_native.set_people_per_helper(256)
    try:
        threads, _ = sheltersim_native.parallel_state(0)
        assert sheltersim_native.parallel_state(255)[1] == 0
        assert sheltersim_native.parallel_state(1000)[1] == min(threads, 3)
    finally:
        sheltersim_native.set_people_per_helper(before)

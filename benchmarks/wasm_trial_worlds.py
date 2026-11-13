"""The trial worlds of the browser's core (stage 4 of #132, PR 4-5 of #244) and what the native
path gives on them.

For each size (people) of the trial Scenario, the generated one of the Project screen
(`gate_scenarios.generated`, one day, seed 2026), this builds the world at minute 0 as
`run_scenario` sets it up and writes what the core's loader reads of it
(`sheltersim.core.world_export.export_world`) to `DIR/trial-<people>.json`. Then it runs that world
through `TickEngine` with the native kernel for the whole day and writes, to `DIR/expected.json`,
the digests the browser's run must give (those of `tests/support/core_tick.py`: each part of the
world's canonical document and the metric snapshots), with the time the ticks took. Unless
`--skip-project`, it also computes the same Scenario through the Project screen's route
(`ProjectAPI`, `gate_scenarios.project_run`) in a temporary store and records its time, the size
of the Run's directory on disk (Checkpoints and history), and whether its last world has the
native path's digests.

    python benchmarks/wasm_trial_worlds.py --output DIR [--sizes 20 100] [--skip-project]

Run from the repository root with src on PYTHONPATH, the native extension built and
SHELTERSIM_LLM_MODE=offline. The files are generated; they belong outside the repository.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
# The trial Scenario: its sizes (people), its length in minutes and its seed.
SIZES = (20, 100)
MINUTES = 1440
SEED = 2026


def _bytes(directory: Path) -> int:
    return sum(path.stat().st_size for path in directory.rglob("*") if path.is_file())


def native(scenario: Any, output: Path, name: str) -> dict[str, Any]:
    """Write the world at minute 0 as `output/<name>.json`, run it for the whole Scenario through
    `TickEngine`, and return what to expect of it."""
    from support.core_fixtures import run_world
    from support.core_tick import metrics_digest, world_digests

    from sheltersim.core.world_export import export_world

    started = time.perf_counter()
    world, controller, engine = run_world(scenario)
    build_seconds = time.perf_counter() - started
    started = time.perf_counter()
    entry = export_world(world, controller)
    text = json.dumps(entry, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    export_seconds = time.perf_counter() - started
    path = output / f"{name}.json"
    path.write_text(text, encoding="utf-8")
    initial = world_digests(world)
    ticks = scenario.duration_minutes
    started = time.perf_counter()
    for _ in range(ticks):
        engine.step()
    tick_seconds = time.perf_counter() - started
    snapshots = controller.artifacts.metrics.snapshots
    return {
        "file": path.name,
        "entry_bytes": path.stat().st_size,
        "build_seconds": build_seconds,
        "export_seconds": export_seconds,
        "initial": initial,
        "expected": {
            "ticks": ticks,
            "world": world_digests(world),
            "metrics": metrics_digest(snapshots),
            "metric_count": len(snapshots),
            "tick_seconds": tick_seconds,
        },
    }


def project(scenario: Any, directory: Path, world_expected: dict[str, str]) -> dict[str, Any]:
    """The Project screen's route for the same Scenario, in a store under `directory`."""
    from gate_scenarios import project_run, timeline_hashes
    from support.core_tick import world_digests

    root = directory / "projects"
    started = time.perf_counter()
    store = project_run(root, scenario, scenario.duration_minutes)
    seconds = time.perf_counter() - started
    run = store.root / "p" / "runs" / "r"
    world, _ = timeline_hashes(store, "main")
    digests = world_digests(world)
    return {
        "seconds": seconds,
        "minute": world.minute_index,
        "run_bytes": _bytes(run),
        "run_parts_bytes": {
            child.name: _bytes(child) if child.is_dir() else child.stat().st_size
            for child in sorted(run.iterdir())
        },
        "differing_parts": [
            part for part, value in digests.items() if world_expected[part] != value
        ],
    }


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--output", type=Path, required=True, help="directory to write into")
    parser.add_argument("--sizes", type=int, nargs="+", default=list(SIZES), help="people")
    parser.add_argument("--skip-project", action="store_true", help="skip the Project route")
    args = parser.parse_args(argv)
    sys.path.insert(0, str(ROOT / "tests"))
    from gate_scenarios import generated

    args.output.mkdir(parents=True, exist_ok=True)
    worlds: dict[str, Any] = {}
    with tempfile.TemporaryDirectory(prefix="wasm-trial-") as work:
        # The usage ledgers and run_world's artifact directories stay in the temporary directory.
        os.environ["SHELTERSIM_PROJECTS_DIR"] = str(Path(work) / "ledgers")
        os.environ["XDG_STATE_HOME"] = str(Path(work) / "state")
        tempfile.tempdir = work
        try:
            for people in args.sizes:
                name = f"trial-{people}"
                scenario = generated(evacuees=people, minutes=MINUTES, seed=SEED)
                result = {"evacuees": people, **native(scenario, args.output, name)}
                if not args.skip_project:
                    result["project"] = project(
                        scenario, Path(work) / name, result["expected"]["world"]
                    )
                worlds[name] = result
                print(json.dumps({name: result}, indent=1), flush=True)
        finally:
            tempfile.tempdir = None
    document = {
        "scenario": {"generator": "gate_scenarios.generated", "minutes": MINUTES, "seed": SEED},
        "platform": {"python": platform.python_version(), "machine": platform.machine()},
        "worlds": worlds,
    }
    (args.output / "expected.json").write_text(
        json.dumps(document, sort_keys=True, indent=1) + "\n", encoding="utf-8"
    )


if __name__ == "__main__":
    main()

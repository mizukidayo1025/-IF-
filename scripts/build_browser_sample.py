"""Build the precomputed sample Project in the browser's form: one bundle file (#265).

    SHELTERSIM_LLM_MODE=offline python scripts/build_browser_sample.py OUTPUT.shelterproj \\
        --pkg PKG_DIR [--inputs DIR] [--ticks 1440]

PKG_DIR is the output of `wasm-bindgen --target nodejs` for sheltersim_wasm.wasm
(docs/development.md). The sample is the main branch of the VM's sample
(scripts/build_project_sample.py): the Scenario the offline parser makes from the new UI's
example text (100 synthetic people, 墨田区, earthquake, 7 days, seed 2026), in Project `sample`,
Run `run-sample-toilet-outage`, branch `main`. The branch forked at minute 5160 is added later
(#251, #250, #267).

Python does what only Python does now (route A of #244): it previews the Scenario from the text,
saves it and creates the Run in a temporary ProjectStore, reads back the records Python's server
answers with and its answer on the Run's evidence route (`evidence_context` and the catalogs the
route adds), builds the world at minute 0 as a Run sets it up, and exports the
world (`sheltersim.core.world_export.export_world`). It writes them to the inputs directory
(a temporary one unless --inputs). Then Node computes the whole Run with the same WASM and
`native/wasm/js/run_store.mjs` the browser uses, makes the Project's records with
`native/wasm/js/project_store.mjs` and writes the bundle
(`native/wasm/tools/build_project_bundle.mjs`).
No Provider is called, and the regional research is not run, as in build_project_sample.py.

The bundle is generated: keep it out of the repository.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
TOOL = ROOT / "native" / "wasm" / "tools" / "build_project_bundle.mjs"
BRANCH_ID = "main"


def _sample() -> Any:
    sys.path.insert(0, str(ROOT / "scripts"))
    try:
        import build_project_sample
    finally:
        sys.path.remove(str(ROOT / "scripts"))
    return build_project_sample


def _canonical_bytes(value: Any) -> bytes:
    from sheltersim.storage.projects import canonical

    return canonical(value).encode()


def export_inputs(
    directory: Path, *, text: str | None = None, request: dict[str, Any] | None = None
) -> dict[str, Any]:
    """Write what the bundle is made of to `directory`: `inputs.json` (the records Python's server
    holds for the new Run, and where they come from), `scenario.json` (the Scenario's canonical
    JSON, whose sha256 is its hash), `evidence.json` and `export.json` (the world at minute 0).
    Returns inputs.json's content."""
    from sheltersim.core.model_version import ENGINE_VERSION
    from sheltersim.core.world_export import export_world
    from sheltersim.llm.config import LLMMode, LLMSettings
    from sheltersim.storage.projects import ProjectStore, digest
    from sheltersim.web.projects import ProjectAPI

    sys.path.insert(0, str(ROOT / "tests"))
    try:
        from support.core_fixtures import run_world
    finally:
        sys.path.remove(str(ROOT / "tests"))

    sample = _sample()
    if LLMSettings.from_env().effective_mode is not LLMMode.OFFLINE:
        raise RuntimeError("the sample is built offline only; set SHELTERSIM_LLM_MODE=offline")
    text = sample.TEXT if text is None else text
    directory.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="sheltersim-browser-sample-") as scratch:
        store = ProjectStore(Path(scratch) / "projects")
        api = ProjectAPI(store, recover=False)
        api.workspace.research.research = sample._no_research
        try:
            draft, scenario = sample._preview(api, text, request)
            saved = store.save_scenario(
                sample.PROJECT_ID, scenario, approved_hash=draft["approved_hash"]
            )
            store.create_run(
                sample.PROJECT_ID, sample.RUN_ID, saved["scenario_id"], saved["scenario_version"]
            )
            project = store.project(sample.PROJECT_ID)
            run = store.run(sample.PROJECT_ID, sample.RUN_ID)
            # What the server answers on the Run's evidence route: evidence_context of the
            # Scenario with the catalogs the route adds (web/project_services.py).
            evidence = api.get([sample.PROJECT_ID, "runs", sample.RUN_ID, "evidence"], {})
        finally:
            api.close()
        # The Scenario as the server reads it back for the Run's world.
        scenario = store.scenario(
            sample.PROJECT_ID, saved["scenario_id"], saved["scenario_version"]
        )
    scenario_bytes = _canonical_bytes(saved["scenario"])
    if (
        scenario_bytes != _canonical_bytes(json.loads(scenario.canonical_json()))
        or digest(saved["scenario"]) != saved["scenario_hash"]
    ):
        raise RuntimeError("the Scenario's canonical JSON is not the one its hash is of")
    (directory / "scenario.json").write_bytes(scenario_bytes)
    (directory / "evidence.json").write_bytes(_canonical_bytes(evidence))

    with tempfile.TemporaryDirectory(prefix="sheltersim-browser-sample-world-") as scratch:
        # run_world's artifact directory stays in the temporary directory.
        previous, tempfile.tempdir = tempfile.tempdir, scratch
        try:
            world, controller, _ = run_world(scenario)
            entry = export_world(world, controller)
        finally:
            tempfile.tempdir = previous
    (directory / "export.json").write_text(
        json.dumps(entry, sort_keys=True, ensure_ascii=False, separators=(",", ":")),
        encoding="utf-8",
    )
    del entry, world, controller

    inputs = {
        "schema_version": 1,
        "kind": "browser_sample_inputs",
        "project": {key: project[key] for key in ("schema_version", "project_id", "archived")},
        "scenario_record": {key: value for key, value in saved.items() if key != "scenario"},
        "run": {key: value for key, value in run.items() if key != "branches"},
        "branch_id": BRANCH_ID,
        "source": {
            "tool": "scripts/build_browser_sample.py",
            "text": text,
            "request": request,
            "agent_count": len(scenario.profiles),
            "duration_minutes": scenario.duration_minutes,
            "seed": scenario.seed,
            "engine_version": ENGINE_VERSION,
            "regional_research": "not_run",
            "note": sample.NOTE,
        },
    }
    (directory / "inputs.json").write_text(
        json.dumps(inputs, ensure_ascii=False, sort_keys=True, indent=1) + "\n", encoding="utf-8"
    )
    return inputs


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("output", type=Path, help="the bundle file to write")
    parser.add_argument(
        "--pkg", type=Path, required=True, help="the output of wasm-bindgen --target nodejs"
    )
    parser.add_argument("--inputs", type=Path, help="keep the inputs in this directory")
    parser.add_argument("--ticks", type=int, default=1440, help="ticks per advance in WASM")
    arguments = parser.parse_args()
    node = shutil.which("node")
    if node is None:
        print("node is not on PATH", file=sys.stderr)
        return 2
    with tempfile.TemporaryDirectory(prefix="sheltersim-browser-sample-") as scratch:
        # Never a Provider, and never the shared usage ledger or response tape.
        os.environ["SHELTERSIM_LLM_MODE"] = "offline"
        os.environ.pop("ANTHROPIC_API_KEY", None)
        os.environ["XDG_STATE_HOME"] = str(Path(scratch) / "state")
        os.environ["SHELTERSIM_LLM_CACHE_PATH"] = str(Path(scratch) / "llm-tape.json")
        os.environ["SHELTERSIM_PROJECTS_DIR"] = str(Path(scratch) / "ledgers")
        inputs = (arguments.inputs or Path(scratch) / "inputs").resolve()
        export_inputs(inputs)
        command = [
            node,
            str(TOOL),
            "--pkg",
            str(arguments.pkg.resolve()),
            "--inputs",
            str(inputs),
            "--output",
            str(arguments.output.resolve()),
            "--ticks",
            str(arguments.ticks),
        ]
        return subprocess.run(command, check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())

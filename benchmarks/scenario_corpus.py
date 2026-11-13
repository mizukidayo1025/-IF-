"""Write the scenario corpus again, or a case's full bytes (Issue #293).

The corpus (`native/core/tests/data/scenario_corpus.json`, built by
`tests/support/scenario_corpus.py`) is what the Python server answers to the Scenario screen
offline, the yardstick of the browser's port (#291). `tests/integration/test_scenario_corpus.py`
checks each case against what the server makes today; run this after a change that moves them.

Run from the repository root with the native extension built:
    SHELTERSIM_LLM_MODE=offline python benchmarks/scenario_corpus.py [CASE ...]
With no case, every case is made again; with cases, only those (the others stay as committed).

    python benchmarks/scenario_corpus.py --dump DIR CASE [CASE ...]
writes each case's full bytes to DIR/<case>/ for debugging a port: `body.json` (what the screen
posts), `answer.json` (the preview's answer without draft_id and preview_id), `scenario.json`
(the Scenario's canonical JSON; its sha256 is approved_hash), `facility_design.json`,
`research.json`, and when the Scenario can start `model_versions.json`, `evidence.json` and
`export.json` (the minute-0 world as the browser sample writes it).
"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def dump(directory: Path, names: list[str]) -> None:
    from support.scenario_corpus import _bytes, case_outputs

    for name in names:
        outputs = case_outputs(name)
        target = directory / name
        target.mkdir(parents=True, exist_ok=True)
        files = {
            "body.json": _bytes(outputs["body"]),
            "answer.json": _bytes(outputs["answer"]),
            "scenario.json": outputs.get("scenario_bytes"),
            "facility_design.json": outputs.get("facility_design_bytes"),
            "research.json": outputs.get("research_bytes"),
            "model_versions.json": _bytes(outputs["model_versions"])
            if "model_versions" in outputs
            else None,
            "evidence.json": outputs.get("evidence_bytes"),
            "export.json": outputs.get("export_bytes"),
        }
        for file, data in files.items():
            if data is not None:
                (target / file).write_bytes(data)
        print(f"wrote {target} ({', '.join(f for f, d in files.items() if d is not None)})")


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("cases", nargs="*")
    parser.add_argument("--dump", type=Path, metavar="DIR")
    arguments = parser.parse_args()
    sys.path.insert(0, str(ROOT / "tests"))
    from support.scenario_corpus import CORPUS_PATH, case_names, corpus_document, render

    unknown = sorted(set(arguments.cases) - set(case_names()))
    if unknown:
        raise SystemExit(f"no such case: {', '.join(unknown)}")
    if arguments.dump is not None:
        if not arguments.cases:
            raise SystemExit("--dump needs at least one case")
        dump(arguments.dump, arguments.cases)
        return
    CORPUS_PATH.write_text(render(corpus_document(arguments.cases)), encoding="utf-8")
    print(f"wrote {CORPUS_PATH.relative_to(ROOT)} ({CORPUS_PATH.stat().st_size} bytes)")


if __name__ == "__main__":
    main()

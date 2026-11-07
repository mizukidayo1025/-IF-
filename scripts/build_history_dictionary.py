"""Build a preset dictionary for the payloads of the history index (#80).

The dictionary is made from synthetic rows only: the history index of the generated
Scenario that `benchmarks/investigate_performance.py --scenario generated` computes
(the synthetic municipality, an earthquake, 100 evacuees, seed 2026), computed here for
one day with SHELTERSIM_LLM_MODE=offline. Never build it from a Run of real people.

The rows are grouped by kind and by the set of their top-level keys. Each group gets a
part of the dictionary in proportion to its share of the payload bytes, and the groups
are laid out from the smallest share to the largest, since deflate reaches nearer bytes
more cheaply. Within a group whole rows go in the order of their SHA-256 while they fit;
a group whose first row does not fit still gets that one row. The parts are scaled up
together, by hundredths, as far as the whole stays within 32 KiB (the deflate window).
Nothing in the choice depends on a compressor, so the same rows give the same dictionary
on any machine.

src/sheltersim/storage/history_payloads_1.zdict was built with this script at the commit
that added it. Rows compressed with a dictionary need that dictionary byte for byte, so a
listed dictionary is never built again in place: building with later code, which writes
other rows, gives another dictionary, which would be added with a new format byte in
`history_payloads.DICTIONARIES`.

    SHELTERSIM_LLM_MODE=offline python scripts/build_history_dictionary.py OUTPUT_FILE
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import sqlite3
import tempfile
from pathlib import Path

os.environ["SHELTERSIM_LLM_MODE"] = "offline"
os.environ.pop("ANTHROPIC_API_KEY", None)

from sheltersim.core.world import WorldState  # noqa: E402
from sheltersim.experiments import timeline as timeline_module  # noqa: E402
from sheltersim.experiments.branch import BranchRuntime  # noqa: E402
from sheltersim.experiments.initial_scenario import (  # noqa: E402
    InitialScenarioBuilder,
    InitialScenarioRequest,
)
from sheltersim.storage import history_payloads  # noqa: E402
from sheltersim.storage.projects import ProjectStore, digest  # noqa: E402

DURATION_MINUTES = 1440
SIZE = 32 * 1024
# A group smaller than this share of the bytes gets no row unless one fits its part.
LEAST_SHARE = 0.0005


def synthetic_payloads(directory: Path) -> list[tuple[str, bytes]]:
    """(kind, payload) of every row in the index of one computed day, in key order."""
    scenario = (
        InitialScenarioBuilder()
        .build(
            InitialScenarioRequest(
                municipality="Synthetic municipality",
                disaster="earthquake",
                evacuees=100,
                duration_days=DURATION_MINUTES / 1440,
                seed=2026,
            )
        )
        .scenario
    )
    store = ProjectStore(directory)
    store.create_project("p")
    store.save_scenario("p", scenario, approved_hash=digest(json.loads(scenario.canonical_json())))
    store.create_run("p", "r", scenario.id, 1)
    timeline = timeline_module.TimelineStore(store, "p", "r")
    timeline.record(BranchRuntime.from_world(WorldState.from_scenario(scenario)))
    # Index in this process, with payloads as text, so no dictionary is needed to build one.
    timeline_module.BACKGROUND_INDEX_MIN_SAVES = 1 << 30
    history_payloads.encode = lambda text: text  # type: ignore[assignment]
    timeline.advance("main", DURATION_MINUTES)
    path = directory / "p" / "runs" / "r" / "history.sqlite"
    with sqlite3.connect(f"file:{path}?mode=ro", uri=True) as db:
        rows = db.execute("SELECT kind,payload FROM records ORDER BY branch,kind,key").fetchall()
    return [(kind, history_payloads.decode(payload).encode()) for kind, payload in rows]


def build(rows: list[tuple[str, bytes]]) -> bytes:
    groups: dict[tuple[str, ...], list[bytes]] = collections.defaultdict(list)
    for kind, payload in rows:
        groups[(kind, *sorted(json.loads(payload)))].append(payload)
    total = sum(len(payload) for _, payload in rows)
    share = {group: sum(map(len, payloads)) / total for group, payloads in groups.items()}
    ordered = {
        group: sorted(set(groups[group]), key=lambda payload: hashlib.sha256(payload).digest())
        for group in sorted(groups, key=lambda group: (share[group], group))
    }

    def laid_out(scale: float) -> bytes:
        parts = []
        for group, payloads in ordered.items():
            budget = int(SIZE * scale * share[group])
            part = b""
            for payload in payloads:
                if len(part) + len(payload) > budget:
                    break
                part += payload
            if not part and share[group] > LEAST_SHARE:
                part = payloads[0]
            parts.append(part)
        return b"".join(parts)

    # Whole rows fill the parts short of their budgets; scale the budgets up, by
    # hundredths, as far as everything still fits.
    for percent in range(200, 100, -1):
        dictionary = laid_out(percent / 100)
        if len(dictionary) <= SIZE:
            return dictionary
    return laid_out(1.0)[-SIZE:]


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("output", type=Path)
    output = parser.parse_args().output
    with tempfile.TemporaryDirectory() as directory:
        rows = synthetic_payloads(Path(directory))
    dictionary = build(rows)
    output.write_bytes(dictionary)
    sha256 = hashlib.sha256(dictionary).hexdigest()
    print(f"{len(rows)} rows, {len(dictionary)} bytes, sha256 {sha256}")


if __name__ == "__main__":
    main()

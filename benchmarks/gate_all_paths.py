"""The all-paths gate: small offline scenarios that together reach most of the tick's code.

Each scenario runs in its own process and writes the world, events and KPI hashes of every
world it ended with, plus timing. Hashes are compared with --expect; --determinism also
runs every scenario under other process settings and compares them with the first run. A write
to model_configs in place inside a tick phase is a problem too (native/src/core/configs.rs).
With --verify-rows, each tick's end also checks that every entity object holds what its row holds,
and that every attached tree of a slot holds what its row holds (core/state_mirror.py,
watch_entity_rows); a tick where they differ is a problem.
With --poison, every object a rule's transaction (or the copy it stands for) takes out of the world
gets a class that raises on any use (core/txn_checks.py); a use is a problem.
With --refuse-transactions, the rules take the copy they made before transactions in place
(txn_checks.refuse_transactions). Each result.json also holds the dispatcher's counts (how many
due events went to the Python dispatcher because a copy or a transaction replaced them) and how many
rule transactions ran in place or on a copy.

    python benchmarks/gate_all_paths.py --output DIR --expect fixtures/gate-all-paths.json
    python benchmarks/gate_all_paths.py --output DIR --determinism
    python benchmarks/gate_all_paths.py --output DIR --reversed-order
    python benchmarks/gate_all_paths.py --output DIR --coverage
    python benchmarks/gate_all_paths.py --output DIR --verify-rows
    python benchmarks/gate_all_paths.py --output DIR --poison [--refuse-transactions]
    python benchmarks/gate_all_paths.py --output DIR --write-expected fixtures/gate-all-paths.json

Run from the repository root with src on PYTHONPATH. Output belongs outside the repository.
"""

from __future__ import annotations

import argparse
import json
import os
import platform
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from pathlib import Path
from typing import Any

HASHES = ("world_hash", "events_hash", "metrics_hash")
# The code the tick runs; coverage is reported for these.
TICK_SIDE = (
    "sheltersim/core/",
    "sheltersim/domain/",
    "sheltersim/events/",
    "sheltersim/policies/",
    "sheltersim/experiments/metrics.py",
    "sheltersim/experiments/operational_kpis.py",
    "sheltersim/experiments/runner.py",
    "sheltersim/experiments/phases/",
)


@dataclass(frozen=True)
class Variant:
    """One way to run a scenario's process."""

    name: str
    env: dict[str, str]


# Each person's compute of a tick goes to helper threads from this many people on
# (native/src/core/parallel.rs); a gate Scenario has fewer, so a variant sets 1 to send everyone.
PEOPLE_PER_HELPER_VARIABLE = "SHELTERSIM_GATE_PEOPLE_PER_HELPER"
# PYTHONHASHSEED varies set and str-hash order; RAYON_NUM_THREADS varies the parallel parse of
# saved records and the helpers of the tick: one thread has none, four have three.
BASE = Variant("base", {"PYTHONHASHSEED": "1"})
VARIANTS = (
    Variant("second_process", {"PYTHONHASHSEED": "1"}),
    Variant("hash_seed", {"PYTHONHASHSEED": "271828"}),
    Variant("one_worker", {"PYTHONHASHSEED": "2", "RAYON_NUM_THREADS": "1"}),
    Variant(
        "four_workers",
        {"PYTHONHASHSEED": "3", "RAYON_NUM_THREADS": "4", PEOPLE_PER_HELPER_VARIABLE: "1"},
    ),
)
# With the base run's hash seed, so that only the order of the dicts differs from it.
ORDER_VARIABLE = "SHELTERSIM_GATE_REVERSE"
ORDER_VARIANTS = (Variant("reversed_order", {"PYTHONHASHSEED": "1", ORDER_VARIABLE: "phase"}),)
# Set by --verify-rows for the scenarios' processes.
VERIFY_ROWS_VARIABLE = "SHELTERSIM_GATE_VERIFY_ROWS"
# Set by --poison and --refuse-transactions.
POISON_VARIABLE = "SHELTERSIM_GATE_POISON"
REFUSE_VARIABLE = "SHELTERSIM_GATE_REFUSE_TRANSACTIONS"


def run_one(name: str, output: Path, chosen: Any = None) -> None:
    """In a scenario's own process: run it and write result.json. Fails closed on network.

    ``chosen`` is a GateScenario to run instead of the one by that name.
    """
    import socket

    os.environ["SHELTERSIM_LLM_MODE"] = "offline"
    os.environ.pop("ANTHROPIC_API_KEY", None)
    state = output / "state"
    os.environ["SHELTERSIM_PROJECTS_DIR"] = str(state / "projects")
    os.environ["XDG_STATE_HOME"] = str(state / "xdg")
    os.environ["SHELTERSIM_LLM_CACHE_PATH"] = str(state / "llm-tape.json")

    def denied(*_args: Any, **_kwargs: Any) -> None:
        raise AssertionError("network forbidden by the offline gate")

    socket.socket.connect = denied  # type: ignore[method-assign]
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from gate_scenarios import all_scenarios

    scenario = chosen or all_scenarios()[name]
    import sheltersim_native

    people_per_helper = os.environ.get(PEOPLE_PER_HELPER_VARIABLE)
    if people_per_helper:
        sheltersim_native.set_people_per_helper(int(people_per_helper))
    # The kernels see an edit of model_configs at the next read, the Python scopes of a tick phase
    # at the next minute (decision U1, native/src/core/configs.rs). Nothing should make one. While
    # it counts, a geometry plan kept by the overlay's stamp is also checked against the overlay
    # (native/src/rules/movement_geometry.rs): every write must give the overlay a new stamp.
    watch_config_writes = getattr(sheltersim_native, "watch_config_writes", None)
    if watch_config_writes is not None:
        watch_config_writes(True)
    verify_rows = bool(os.environ.get(VERIFY_ROWS_VARIABLE))
    if verify_rows:
        from sheltersim.core.state_mirror import watch_entity_rows

        watch_entity_rows(True)
    poison = bool(os.environ.get(POISON_VARIABLE))
    if poison:
        from sheltersim.core.txn_checks import poison_replaced

        poison_replaced(True)
    if os.environ.get(REFUSE_VARIABLE):
        from sheltersim.core.txn_checks import refuse_transactions

        refuse_transactions(True)
    order = os.environ.get(ORDER_VARIABLE)
    if order:
        from tick_probe import ReversedOrder, install

        install([ReversedOrder()])
    started = time.perf_counter()
    outcome = scenario.run(output / "work")
    mismatched = [
        {"first": first, "second": second, "means": means}
        for first, second, means in outcome.must_match
        if any(outcome.runs[first][key] != outcome.runs[second][key] for key in HASHES)
    ]
    (output / "result.json").write_text(
        json.dumps(
            {
                "scenario": name,
                "what": scenario.what,
                "wall_seconds": time.perf_counter() - started,
                "runs": outcome.runs,
                "refused": outcome.refused,
                "must_match": [list(row) for row in outcome.must_match],
                "must_match_failed": mismatched,
                "config_writes_in_tick": (
                    None
                    if watch_config_writes is None
                    else sheltersim_native.config_writes_in_tick()
                ),
                "stale_geometry_hits": getattr(
                    sheltersim_native, "stale_geometry_hits", lambda: None
                )(),
                "entity_rows": _entity_rows() if verify_rows else None,
                "slot_rows": _slot_rows() if verify_rows else None,
                "poison": _poison_uses() if poison else None,
                "dispatch": dict(sheltersim_native.rules.dispatch.stats()[0]),
                "transactions": _transactions(sheltersim_native),
            },
            ensure_ascii=False,
            indent=2,
        )
        + "\n",
        encoding="utf-8",
    )


def _entity_rows() -> dict[str, Any]:
    from sheltersim.core.state_mirror import entity_row_mismatches

    return entity_row_mismatches()


def _slot_rows() -> dict[str, Any]:
    from sheltersim.core.state_mirror import slot_row_mismatches

    return slot_row_mismatches()


def _poison_uses() -> dict[str, Any]:
    from sheltersim.core.txn_checks import poison_uses

    return poison_uses()


def _transactions(native: Any) -> dict[str, int] | None:
    """What the rule transactions did (native/src/rules/transaction.rs), or None before them."""
    counts = getattr(getattr(native.rules, "transaction", None), "counts", None)
    return None if counts is None else dict(counts())


def _spawn(
    name: str, directory: Path, variant: Variant, coverage_file: Path | None
) -> dict[str, Any]:
    """Run a scenario in a new process; a crash is a result, with its log kept."""
    directory.mkdir(parents=True, exist_ok=False)
    env = {**os.environ, **variant.env, "SHELTERSIM_LLM_MODE": "offline"}
    command = [sys.executable]
    if coverage_file is not None:
        rc = directory / "coverage.rc"
        rc.write_text("[run]\nsource = sheltersim\nbranch = False\ncore = sysmon\n", "utf-8")
        command += ["-m", "coverage", "run", f"--rcfile={rc}", f"--data-file={coverage_file}"]
    command += [__file__, "--run-one", name, "--output", str(directory)]
    started = time.perf_counter()
    with (directory / "log.txt").open("w", encoding="utf-8") as log:
        code = subprocess.run(
            command, env=env, stdout=log, stderr=subprocess.STDOUT, check=False
        ).returncode
    result_file = directory / "result.json"
    if code != 0 or not result_file.exists():
        tail = (directory / "log.txt").read_text(encoding="utf-8")[-2000:]
        return {"scenario": name, "crashed": True, "exit_code": code, "log_tail": tail}
    result = json.loads(result_file.read_text(encoding="utf-8"))
    result["process_seconds"] = time.perf_counter() - started
    return result


def _scenario_names(only: list[str] | None) -> list[str]:
    sys.path.insert(0, str(Path(__file__).resolve().parent))
    from gate_scenarios import all_scenarios

    names = list(all_scenarios())
    if not only:
        return names
    chosen = [name for name in names if any(name.startswith(prefix) for prefix in only)]
    if not chosen:
        raise SystemExit(f"no scenario matches {only}")
    return chosen


def _run_all(
    names: list[str],
    output: Path,
    variant: Variant,
    jobs: int,
    coverage_dir: Path | None = None,
) -> dict[str, dict[str, Any]]:
    def one(name: str) -> dict[str, Any]:
        data = coverage_dir / f"{variant.name}.{name}.cov" if coverage_dir else None
        result = _spawn(name, output / variant.name / name, variant, data)
        state = "CRASH" if result.get("crashed") else f"{result['wall_seconds']:.1f}s"
        print(f"[{variant.name}] {name}: {state}", flush=True)
        return result

    with ThreadPoolExecutor(max_workers=jobs) as pool:
        return dict(zip(names, pool.map(one, names), strict=True))


def _problems_within(results: dict[str, dict[str, Any]]) -> list[str]:
    problems = []
    for name, result in results.items():
        if result.get("crashed"):
            problems.append(f"{name}: crashed (exit {result['exit_code']}): {result['log_tail']}")
            continue
        for row in result["must_match_failed"]:
            problems.append(
                f"{name}: {row['first']} and {row['second']} differ, but {row['means']}"
            )
        if result.get("config_writes_in_tick"):
            problems.append(
                f"{name}: model_configs was written in place inside a tick phase "
                f"{result['config_writes_in_tick']} times"
            )
        if result.get("stale_geometry_hits"):
            problems.append(
                f"{name}: a geometry plan was kept for an overlay stamp after the overlay changed "
                f"{result['stale_geometry_hits']} times"
            )
        rows = result.get("entity_rows")
        if rows and rows["mismatches"]:
            problems.append(
                f"{name}: the entity objects differed from their rows at {rows['mismatches']} of "
                f"{rows['checked']} tick ends; first {rows['first']}"
            )
        slots = result.get("slot_rows")
        if slots and slots["mismatches"]:
            problems.append(
                f"{name}: a tree of a slot differed from its row at {slots['mismatches']} of "
                f"{slots['checked']} tick ends; first {slots['first']}"
            )
        poisoned = result.get("poison")
        if poisoned and poisoned["uses"]:
            problems.append(
                f"{name}: an object taken out of the world was used {poisoned['uses']} times; "
                f"first {poisoned['first']}"
            )
    return problems


def _differences(label: str, first: dict[str, Any], second: dict[str, Any]) -> list[str]:
    """What differs between two results of one scenario, by run and hash."""
    if first.get("crashed") or second.get("crashed"):
        return [f"{label}: crashed"]
    found = []
    for run in sorted(set(first["runs"]) | set(second["runs"])):
        a, b = first["runs"].get(run), second["runs"].get(run)
        if a is None or b is None:
            found.append(f"{label}/{run}: present on one side only")
            continue
        different = [key for key in HASHES if a[key] != b[key]]
        if different:
            found.append(f"{label}/{run}: {', '.join(different)}")
    if first.get("refused") != second.get("refused"):
        found.append(f"{label}: the refused interventions differ")
    return found


def _expected_document(results: dict[str, dict[str, Any]]) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "scenarios": {
            name: {"runs": result["runs"], "refused": result["refused"]}
            for name, result in sorted(results.items())
        },
    }


def _tick_side_rows(data_files: list[Path]) -> list[dict[str, Any]]:
    """Per file: the tick-side lines that can run and those that did, over all data files."""
    import coverage

    lines: dict[str, set[int]] = {}
    for data_file in data_files:
        part = coverage.CoverageData(basename=str(data_file))
        part.read()
        for filename in part.measured_files():
            lines.setdefault(filename, set()).update(part.lines(filename) or ())
    measured = coverage.Coverage(data_file=None, config_file=False, source=["sheltersim"])
    measured.get_data().add_lines({name: sorted(found) for name, found in lines.items()})
    rows = {}
    root = Path(__import__("sheltersim").__file__).parent
    # Files no scenario imported count as not reached at all.
    for path in sorted(root.rglob("*.py")):
        short = "sheltersim/" + path.relative_to(root).as_posix()
        if short.startswith(TICK_SIDE):
            _, statements, _, missing, _ = measured.analysis2(str(path))
            rows[short] = {
                "file": short,
                "statements": len(statements),
                "missing": len(missing),
                "covered": len(statements) - len(missing),
                "missing_lines": missing,
            }
    return [rows[key] for key in sorted(rows)]


def _write_coverage(rows: list[dict[str, Any]], path: Path) -> None:
    total = sum(row["statements"] for row in rows)
    covered = sum(row["covered"] for row in rows)
    lines = [
        "| file | lines | reached | % |",
        "|---|---:|---:|---:|",
        *(
            f"| {row['file']} | {row['statements']} | {row['covered']} | "
            f"{100 * row['covered'] / max(row['statements'], 1):.0f} |"
            for row in rows
        ),
        f"| total | {total} | {covered} | {100 * covered / max(total, 1):.1f} |",
    ]
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    path.with_suffix(".json").write_text(json.dumps(rows, indent=1) + "\n", encoding="utf-8")
    print(f"tick-side coverage: {covered}/{total} lines ({100 * covered / max(total, 1):.1f}%)")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n", 1)[0])
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--expect", type=Path, help="fixture of expected hashes to compare with")
    parser.add_argument("--write-expected", type=Path, help="write the results as that fixture")
    parser.add_argument("--determinism", action="store_true", help="also run the other variants")
    parser.add_argument(
        "--reversed-order",
        action="store_true",
        help="also reverse model_state's dicts and world.agents at each phase's end",
    )
    parser.add_argument(
        "--coverage",
        action="store_true",
        help="measure the tick-side Python lines the base runs reach, into coverage.md",
    )
    parser.add_argument(
        "--verify-rows",
        action="store_true",
        help="check the entity objects and the slot trees against their rows at every tick's end",
    )
    parser.add_argument(
        "--poison",
        action="store_true",
        help="make every object a rule's transaction takes out of the world raise on any use",
    )
    parser.add_argument(
        "--refuse-transactions",
        action="store_true",
        help="make the rules take their copy instead of a transaction in place",
    )
    parser.add_argument("--only", action="append", help="scenarios whose name starts with this")
    parser.add_argument("--jobs", type=int, default=min(os.cpu_count() or 1, 12))
    parser.add_argument("--run-one", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.run_one:
        run_one(args.run_one, args.output)
        return 0

    args.output.mkdir(parents=True, exist_ok=False)
    if args.verify_rows:
        os.environ[VERIFY_ROWS_VARIABLE] = "1"
    if args.poison:
        os.environ[POISON_VARIABLE] = "1"
    if args.refuse_transactions:
        os.environ[REFUSE_VARIABLE] = "1"
    started = time.perf_counter()
    names = _scenario_names(args.only)
    coverage_dir = args.output / "coverage" if args.coverage else None
    if coverage_dir:
        coverage_dir.mkdir()
    results = _run_all(names, args.output, BASE, args.jobs, coverage_dir)
    problems = _problems_within(results)
    summary: dict[str, Any] = {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "scenarios": {
            name: {
                "wall_seconds": result.get("wall_seconds"),
                "what": result.get("what"),
                "runs": result.get("runs"),
                "refused": result.get("refused"),
                "crashed": bool(result.get("crashed")),
                "entity_rows": result.get("entity_rows"),
                "slot_rows": result.get("slot_rows"),
                "poison": result.get("poison"),
                "dispatch": result.get("dispatch"),
                "transactions": result.get("transactions"),
            }
            for name, result in results.items()
        },
    }

    if args.expect:
        expected = json.loads(args.expect.read_text(encoding="utf-8"))["scenarios"]
        for name in names:
            if name not in expected:
                problems.append(f"{name}: not in {args.expect}")
            else:
                problems.extend(_differences(f"expected {name}", expected[name], results[name]))

    if args.determinism or args.reversed_order:
        table: dict[str, dict[str, Any]] = {name: {} for name in names}
        variants = list(VARIANTS) if args.determinism else []
        if args.reversed_order:
            variants += ORDER_VARIANTS
        for variant in variants:
            other = _run_all(names, args.output, variant, args.jobs, coverage_dir)
            problems.extend(_problems_within(other))
            for name in names:
                differences = _differences(f"{variant.name} {name}", results[name], other[name])
                problems.extend(differences)
                table[name][variant.name] = "same" if not differences else "DIFFERENT"
        summary["determinism"] = table

    if coverage_dir:
        base = sorted(coverage_dir.glob(f"{BASE.name}.*.cov"))
        _write_coverage(_tick_side_rows(base), args.output / "coverage.md")

    if args.write_expected:
        if problems:
            print("not writing expected hashes: there are problems", file=sys.stderr)
        else:
            args.write_expected.write_text(
                json.dumps(_expected_document(results), ensure_ascii=False, indent=1) + "\n",
                encoding="utf-8",
            )

    summary["problems"] = problems
    summary["total_seconds"] = time.perf_counter() - started
    (args.output / "summary.json").write_text(
        json.dumps(summary, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
    )
    print(f"{len(names)} scenarios in {summary['total_seconds']:.1f}s; {len(problems)} problems")
    if args.verify_rows:
        rows = [result.get("entity_rows") or {} for result in results.values()]
        checked = sum(row.get("checked", 0) for row in rows)
        differed = sum(row.get("mismatches", 0) for row in rows)
        print(f"entity rows checked at {checked} tick ends; {differed} differed")
        slots = [result.get("slot_rows") or {} for result in results.values()]
        print(
            f"slot trees checked at {sum(row.get('checked', 0) for row in slots)} tick ends "
            f"({sum(row.get('trees', 0) for row in slots)} trees); "
            f"{sum(row.get('mismatches', 0) for row in slots)} differed"
        )
    if args.poison:
        poisoned = [result.get("poison") or {} for result in results.values()]
        print(
            f"{sum(row.get('poisoned', 0) for row in poisoned)} objects poisoned; "
            f"{sum(row.get('uses', 0) for row in poisoned)} uses"
        )
    dispatch = [result.get("dispatch") or {} for result in results.values()]
    transactions = [result.get("transactions") or {} for result in results.values()]
    print(
        "events to the Python dispatcher: "
        f"{sum(row.get('events_python', 0) for row in dispatch)}, of which replaced: "
        f"{sum(row.get('events_replaced', 0) for row in dispatch)}; rule transactions in place: "
        f"{sum(row.get('in_place', 0) for row in transactions)}, on a copy: "
        f"{sum(row.get('copy', 0) for row in transactions)}"
    )
    for problem in problems:
        print(f"PROBLEM {problem}")
    return 1 if problems else 0


if __name__ == "__main__":
    raise SystemExit(main())

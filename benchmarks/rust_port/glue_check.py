"""The glue check of Issue #152: the PyO3 crate's rules files are glue, and no Python rule is left.

Design-p9.md section 6 (and design-p8.md section 4.1, items 1 and 2) says what
native/src/rules/*.rs may hold once the rules are the core's: Python entries that read their
arguments, build the world's cell and the native host, make one call of `sheltersim_core`, and
convert what it gives back; the objects made for Python callers; and the E10 detection of the tick
and the dispatch. Every other Python call lives in native/src/host.rs or native/src/core/.

This script finds, in each rules file (but the exempt files):

- the state shims and the rule arithmetic of the deny list (`with_rows`, `EventWriter`,
  `namespace_held`, `configs::kept`, `py_fsum`, `std::time`, ...);
- the calls into Python (`call_method*`, `.call(`, `.call0(`, `.call1(`);
- each `with_world(` closure that is not a single expression.

Each finding must be named in `EXEMPTIONS` with the number of times it occurs in that file and the
reason it stays; a finding not named, or a count that differs (an exemption that is stale), fails.
It also checks that the Python fallbacks deleted in #152 stay deleted (`PYTHON_GONE`), that the
steps whose rules moved are body-less `kernel_only` entries (`BODYLESS`), and that the dispatcher's
stats no longer name the Python paths (`STATS_GONE`).

Run: python benchmarks/rust_port/glue_check.py (exit 0 when clean). CI runs it in the lint job.
"""

from __future__ import annotations

import ast
import re
import sys
from collections.abc import Iterable, Mapping
from dataclasses import dataclass
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
RULES = ROOT / "native" / "src" / "rules"
PYTHON = ROOT / "src" / "sheltersim"

# Files whose whole content is exempt, with the reason.
EXEMPT_FILES: Mapping[str, str] = {
    "parity.rs": "the parity tests' entries, which compare the Python and Rust readers",
    "after_tick.rs": "E6: the end-of-tick validators read what Python holds (the d4 decision)",
    "tick_validation.rs": "E6: the validators' glue reads what Python holds (the d4 decision)",
    "movement_geometry.rs": "H2: the floor plan's parse fillers, which the native host runs",
}

# The deny list (design-p8.md 4.1-(1) and design-p9.md section 6), by name.
DENY: Mapping[str, str] = {
    "with_rows": r"\bwith_rows\b",
    "entities_of": r"\bentities_of\b",
    "EntityStore::": r"\bEntityStore::",
    "mirror_of": r"\bmirror_of\b",
    "field_mirror_of": r"\bfield_mirror_of\b",
    "held_state": r"\bheld_state\b",
    "with_state_value": r"\bwith_state_value\b",
    "attached_state": r"\battached_state\b",
    "read_only_state": r"\bread_only_state\b",
    "with_row": r"\bwith_row\b",
    "with_value": r"\bwith_value\b",
    "write_through": r"\bwrite_through\b",
    "EventWriter": r"\bEventWriter\b",
    "raw_setattr": r"\braw_setattr\b",
    "rows_mut": r"\brows_mut\b",
    "KeyedDraws": r"\bKeyedDraws\b",
    "py_sum": r"\bpy_sum\b",
    "py_fsum": r"\bpy_fsum\b",
    "mathx": r"\bmathx::(?:exp|expm1|pow|hypot|py_round)\b",
    "namespace_held": r"\bnamespace_held\b",
    "field_held": r"\bfield_held\b",
    "namespace_is_absent": r"\bnamespace_is_absent\b",
    "configs::kept": r"\bconfigs::kept",
    "controller.call_method": r"\bcontroller\.call_method",
    "std::time": r"\bstd::time\b|\bInstant\b",
    # Calls into Python: conversion only (a class made for a Python caller), else the host's.
    "python call": r"\.call_method\w*\(|\.call[01]?\(",
}

# A `with_world(` closure that is not a single expression.
CLOSURE = "with_world block"

# Why a finding stays (the categories of the exemptions below).
SECTION = (
    "H2/E15: the Python parse of a config section (its contract or factory), which"
    " PyHost::fill_section and read_again run at the read site's first read"
)
CONVERSION = (
    "conversion: the Python object a Python caller gets (a class made from the core's values)"
)
ARGUMENT = "reads a Python argument or a Python mapping it was given (the entry's input)"
QUEUE = (
    "H6: the Python objects of pending events (QueueStore), which a Python caller scheduled or"
    " holds; the core reads rows (read_queue_rows) and lets objects go (slots_changed)"
)
ACTIONS = (
    "the open actions' store (`ActiveActions`, a pyclass) lent as the Run's rows for one core call"
)
E10 = "E10: the checks before anything changes, which send a tick to the phase path"
KPI = (
    "`operational_kpi_values` (inspection traces, not a tick step) still reads the rows in the"
    " PyO3 crate; moving it to the core is P10 work"
)
HOOK = "a Python hook a test or a caller set (H4/H5): its own `after_complete`, `request`, sink"

# Findings that stay, by (file, finding): (times, reason). Keep each reason true for its entry.
EXEMPTIONS: Mapping[tuple[str, str], tuple[int, str]] = {
    ("action_lifecycle.rs", "with_rows"): (6, ACTIONS),
    ("action_lifecycle.rs", "EventWriter"): (3, QUEUE),
    ("behavior.rs", "raw_setattr"): (1, CONVERSION + " (`object.__new__` and its `__dict__`)"),
    ("behavior.rs", "python call"): (2, CONVERSION + " (`ActionKind` members, `object.__new__`)"),
    ("body_state.rs", "configs::kept"): (1, SECTION),
    ("body_state.rs", "python call"): (1, SECTION),
    ("care_receipts.rs", "python call"): (1, CONVERSION + " (`EventLogRecord`)"),
    ("common_operations.rs", "python call"): (
        2,
        "the EventLog's Python records (`event_log.find`, PyHost::find_record) and a receipt's"
        " contract dump (E15)",
    ),
    ("decisions.rs", "with_rows"): (1, ACTIONS),
    ("decisions.rs", "python call"): (4, HOOK + ": a proposal Python made, and a Python sink (H5)"),
    ("dispatch.rs", "with_rows"): (1, E10),
    ("dispatch.rs", "EventWriter"): (1, E10),
    ("effects.rs", "configs::kept"): (1, SECTION),
    ("effects.rs", "python call"): (2, SECTION + " (`ScenarioZoneEnvironmentConfig`)"),
    ("facility_queues.rs", "EventWriter"): (1, QUEUE),
    ("facility_queues.rs", "python call"): (1, CONVERSION + " (a copy of the request's record)"),
    ("handlers.rs", "configs::kept"): (3, SECTION),
    ("handlers.rs", "python call"): (7, SECTION + "; and `EventLogRecord` for a Python caller"),
    ("information_events.rs", "configs::kept"): (1, SECTION),
    ("information_events.rs", "python call"): (1, SECTION + " (`InformationSpreadConfig`)"),
    ("metrics.rs", "with_rows"): (1, KPI),
    ("metrics.rs", "entities_of"): (1, KPI),
    ("metrics.rs", "EntityStore::"): (1, KPI),
    ("metrics.rs", "field_mirror_of"): (1, KPI),
    ("metrics.rs", "with_row"): (2, KPI),
    ("metrics.rs", "py_sum"): (1, KPI),
    ("metrics.rs", "namespace_is_absent"): (1, KPI),
    ("metrics.rs", "python call"): (1, CONVERSION + " (`MetricSnapshot`)"),
    ("movement.rs", "python call"): (4, CONVERSION + " (`Point2D`, `ZoneRoute`)"),
    ("population.rs", "python call"): (2, ARGUMENT),
    ("population_lifecycle.rs", "EventWriter"): (1, QUEUE),
    ("population_lifecycle.rs", "configs::kept"): (1, SECTION),
    ("population_lifecycle.rs", "python call"): (
        2,
        SECTION + " (`PopulationPresenceCommand`, E15)",
    ),
    ("psychology.rs", "python call"): (2, CONVERSION + " (`AgentStateDelta`, the result)"),
    ("service.rs", "EventWriter"): (1, QUEUE),
    ("service.rs", "python call"): (2, QUEUE + "; and `ResourceRequestReceipt` (conversion)"),
    ("social_dialogue.rs", "python call"): (1, ARGUMENT),
    ("social_propagation.rs", "python call"): (1, CONVERSION + " (`AgentStateDelta`)"),
    ("staff.rs", "EventWriter"): (1, QUEUE),
    ("staff.rs", "configs::kept"): (1, SECTION + " (the staff operations' formula)"),
    ("staff.rs", "python call"): (1, SECTION + " (`create_staff_operations_formula`)"),
    ("staff_operation_events.rs", "EventWriter"): (1, QUEUE),
    ("support.rs", "EventWriter"): (2, QUEUE),
    ("support.rs", "configs::kept"): (1, SECTION),
    ("support.rs", "python call"): (
        2,
        "the phase path's `staff_service.assign_pending` (E10, its manager's Python entry) and the"
        " care formula's factory (H2)",
    ),
    ("tick.rs", "with_rows"): (1, E10),
    ("toilet.rs", "python call"): (2, HOOK + "; and `ToiletRequestReceipt` (conversion)"),
    ("travel.rs", "python call"): (1, CONVERSION + " (`ZoneRoute`)"),
    ("travel_completion.rs", "EventWriter"): (1, QUEUE),
    ("travel_completion.rs", "python call"): (2, HOOK + " (a manager's own `request`, H4)"),
    ("world_view.rs", "raw_setattr"): (1, CONVERSION + " (`object.__new__` and its `__dict__`)"),
    ("world_view.rs", "python call"): (3, CONVERSION + "; a view Python made (argument); H2"),
    ("zone_environment.rs", "python call"): (
        3,
        CONVERSION + " (`ZoneEnvironmentView` of `model_state.get`, for a Python reader's entry)",
    ),
}

# Python names of the fallbacks and rules #152 deleted: no definition and no use in src/sheltersim.
PYTHON_GONE = (
    "_dispatch_one_event",
    "_finish_stopped_journey",
    "_interrupt_trips_of_event",
    "_admit_pending_returns",
    "care_source_ids_for_agent",
    "record_support_request",
    "record_observed_contacts",
    "effect_event_guard_or_suppress",
    "_arbitrate_independent_facility",
)
PYTHON_GONE_PATTERN = re.compile(r"^make_\w+_record$")

# Steps whose rule moved to the core: each a `kernel_only` entry with no body but its docstring,
# as (file under src/sheltersim, class or None, name).
BODYLESS = (
    ("experiments/phases/dispatch.py", "DispatchPhase", "_sync_movement_after_events"),
    ("experiments/phases/dispatch.py", "DispatchPhase", "_consider_social_contact_event"),
    ("domain/facilities.py", "ToiletServiceManager", "arbitrate"),
    ("domain/resource_service.py", "ResourceServiceManager", "arbitrate"),
    ("events/handlers.py", "EventDispatcher", "_dispatch_one"),
    ("events/handlers.py", "EventDispatcher", "settle"),
)

# The dispatcher's counters of the Python paths, gone with them (lane P9-1c).
STATS_GONE = ("events_python", "settled_python", "finished_python", "sync_refused")


@dataclass(frozen=True)
class Finding:
    file: str
    what: str
    lines: tuple[int, ...]


def _strip_comments(text: str) -> str:
    """The text with `//` comments and `use` items blanked (line numbers kept): only what the code
    does counts, not what it imports. Strings are left as they are."""
    out = []
    in_use = False
    for line in text.splitlines():
        stripped = line.lstrip()
        if in_use or re.match(r"(pub(\(crate\))? )?use ", stripped):
            in_use = ";" not in line
            out.append("")
            continue
        if stripped.startswith("//"):
            out.append("")
            continue
        index = line.find(" //")
        out.append(line[:index] if index >= 0 and line.count('"', 0, index) % 2 == 0 else line)
    return "\n".join(out)


def _closure_blocks(text: str) -> list[int]:
    """The lines of each `with_world(` call whose closure body is a block: `|...| {`."""
    found = []
    for match in re.finditer(r"\bwith_world\(", text):
        depth, start = 0, match.end() - 1
        for index in range(start, len(text)):
            char = text[index]
            if char == "(":
                depth += 1
            elif char == ")":
                depth -= 1
                if depth == 0:
                    call = text[start : index + 1]
                    if re.search(r"\|[^|]*\|\s*(->\s*[^{]+)?\{", call):
                        found.append(text.count("\n", 0, match.start()) + 1)
                    break
    return found


def scan_rust(files: Mapping[str, str]) -> list[Finding]:
    """What each file holds of the deny list, its Python calls and its closures."""
    findings = []
    for name, raw in sorted(files.items()):
        if name in EXEMPT_FILES:
            continue
        text = _strip_comments(raw)
        lines = text.splitlines()
        for what, pattern in DENY.items():
            hits = tuple(
                number for number, line in enumerate(lines, 1) for _ in re.finditer(pattern, line)
            )
            if hits:
                findings.append(Finding(name, what, hits))
        blocks = _closure_blocks(text)
        if blocks:
            findings.append(Finding(name, CLOSURE, tuple(blocks)))
    return findings


def judge(
    findings: Iterable[Finding], exemptions: Mapping[tuple[str, str], tuple[int, str]]
) -> list[str]:
    """What fails: a finding not exempt, a count unlike its exemption's, an exemption unused."""
    problems = []
    seen = set()
    for finding in findings:
        key = (finding.file, finding.what)
        seen.add(key)
        allowed = exemptions.get(key)
        count = len(finding.lines)
        lines = ", ".join(map(str, finding.lines))
        if allowed is None:
            problems.append(f"{finding.file}: {finding.what} x{count} (lines {lines}) not allowed")
        elif allowed[0] != count:
            problems.append(
                f"{finding.file}: {finding.what} x{count} (lines {lines}), "
                f"but the exemption says {allowed[0]}"
            )
    for key in sorted(set(exemptions) - seen):
        problems.append(f"{key[0]}: {key[1]} is exempt but no longer found: drop the exemption")
    return problems


def _is_kernel_only(node: ast.FunctionDef) -> bool:
    for decorator in node.decorator_list:
        called = decorator.func if isinstance(decorator, ast.Call) else decorator
        if isinstance(called, ast.Name) and called.id == "kernel_only":
            return True
    return False


def _bodyless(node: ast.FunctionDef) -> bool:
    body = node.body
    return (
        len(body) == 1
        and isinstance(body[0], ast.Expr)
        and isinstance(body[0].value, ast.Constant)
        and isinstance(body[0].value.value, str)
    )


def scan_python(sources: Mapping[str, str]) -> list[str]:
    """The deleted Python names still defined or used, and the moved steps that have a body."""
    problems = []
    trees = {name: ast.parse(text, name) for name, text in sources.items()}
    for name, tree in sorted(trees.items()):
        for node in ast.walk(tree):
            used = None
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                used = node.name
            elif isinstance(node, ast.Attribute):
                used = node.attr
            elif isinstance(node, ast.Name):
                used = node.id
            elif isinstance(node, ast.alias):
                used = node.asname or node.name.rsplit(".", 1)[-1]
            if used is not None and (used in PYTHON_GONE or PYTHON_GONE_PATTERN.match(used)):
                line = getattr(node, "lineno", 0)
                problems.append(f"{name}:{line}: {used} is a deleted Python rule or fallback")
    for file, owner, function in BODYLESS:
        tree = trees.get(file)
        if tree is None:
            problems.append(f"{file}: missing")
            continue
        scope: list[ast.stmt] = tree.body
        if owner is not None:
            classes = [n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == owner]
            scope = classes[0].body if classes else []
        defs = [n for n in scope if isinstance(n, ast.FunctionDef) and n.name == function]
        if not defs:
            problems.append(f"{file}: {owner}.{function} is missing")
        elif not (_is_kernel_only(defs[0]) and _bodyless(defs[0])):
            problems.append(f"{file}: {owner}.{function} must be a body-less kernel_only entry")
    return problems


def scan_stats(files: Mapping[str, str]) -> list[str]:
    problems = []
    for name, text in sorted(files.items()):
        for stat in STATS_GONE:
            if re.search(rf"\b{stat}\b", text):
                problems.append(f"{name}: names the deleted dispatcher stat {stat}")
    return problems


def _read(paths: Iterable[Path], root: Path) -> dict[str, str]:
    return {path.relative_to(root).as_posix(): path.read_text("utf-8") for path in paths}


def run(root: Path = ROOT) -> list[str]:
    """Every problem of the tree at `root`."""
    rules = root / "native" / "src" / "rules"
    rust = _read(sorted(rules.glob("*.rs")), rules)
    python = _read(sorted((root / "src" / "sheltersim").rglob("*.py")), root / "src" / "sheltersim")
    native = _read(sorted((root / "native").glob("*/**/*.rs")), root / "native")
    native = {name: text for name, text in native.items() if not name.startswith("target/")}
    problems = judge(scan_rust(rust), EXEMPTIONS)
    problems += scan_python(python)
    problems += scan_stats(native)
    return problems


def main() -> int:
    problems = run()
    for problem in problems:
        print(problem)
    exempt = sum(times for times, _ in EXEMPTIONS.values())
    print(
        f"glue check: {len(problems)} problems; {len(EXEMPTIONS)} exemptions ({exempt} findings)"
        f" in {len({file for file, _ in EXEMPTIONS})} files, {len(EXEMPT_FILES)} exempt files"
    )
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

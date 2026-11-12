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

Each finding must be named in `EXEMPTIONS` with the number of times it occurs in that file, the
classes it stays as and what it is; a finding not named, or a count that differs (an exemption that
is stale), fails. The classes are only those the definition of done allows (`CLASSES`: a conversion
for a Python caller, a read of a Python argument, a Python hook a test or a caller set, and the E10
checks in tick.rs and dispatch.rs), and only the files of `ALLOWED_EXEMPT_FILES` (the parity tests
and the E6 validators) may be exempt whole (lane P10-1c).
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

# Files whose whole content is exempt, with the reason. Only `ALLOWED_EXEMPT_FILES` may be.
EXEMPT_FILES: Mapping[str, str] = {
    "parity.rs": "the parity tests' entries, which compare the Python and Rust readers",
    "after_tick.rs": "E6: the end-of-tick validators read what Python holds (the d4 decision)",
    "tick_validation.rs": "E6: the validators' glue reads what Python holds (the d4 decision)",
}

# The files the definition of done lets be exempt whole (design-p8.md 4.1-(1), design-p9.md
# section 6): the parity tests and the E6 validators.
ALLOWED_EXEMPT_FILES = frozenset({"parity.rs", "after_tick.rs", "tick_validation.rs"})

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

# Why a finding may stay: the classes the definition of done allows (design-p8.md 4.1-(1),
# design-p9.md section 6; the orchestrator's decision for lane P10-1c). An exemption names its
# classes; a class outside these fails the check.
CONVERSION = "CONVERSION"
ARGUMENT = "ARGUMENT"
HOOK = "HOOK"
E10 = "E10"
CLASSES: Mapping[str, str] = {
    CONVERSION: "the Python object a Python caller gets (a class made from the core's values)",
    ARGUMENT: "reads a Python argument or a Python mapping it was given (the entry's input)",
    HOOK: "a Python hook a test or a caller set (H4/H5): its own `after_complete`, `request`, sink",
    E10: "the checks before anything changes, which refuse a tick a hook would change",
}

# The files whose E10 detection may stay (design-p9.md section 6).
E10_FILES = frozenset({"tick.rs", "dispatch.rs"})

# Findings that stay, by (file, finding): (times, classes, what). Keep each entry true.
EXEMPTIONS: Mapping[tuple[str, str], tuple[int, tuple[str, ...], str]] = {
    ("behavior.rs", "raw_setattr"): (1, (CONVERSION,), "`object.__new__` and its `__dict__`"),
    ("behavior.rs", "python call"): (2, (CONVERSION,), "`ActionKind` members, `object.__new__`"),
    ("care_receipts.rs", "python call"): (1, (CONVERSION,), "`EventLogRecord`"),
    ("common_operations.rs", "python call"): (
        1,
        (ARGUMENT,),
        "the receipt a Python caller gives, dumped by its contract (`model_dump`, E15)",
    ),
    ("decisions.rs", "python call"): (
        4,
        (CONVERSION, HOOK),
        "the proposal and the observed state made for a Python reader (`ProposalRow`,"
        " `ObservedRow`); `with_outcome` and `record_decision` of a Python sink (H5)",
    ),
    ("facility_queues.rs", "python call"): (1, (CONVERSION,), "a copy of the request's record"),
    ("handlers.rs", "python call"): (
        2,
        (ARGUMENT, CONVERSION),
        "`payload.get` of an event a Python caller gives; `EventLogRecord`",
    ),
    ("metrics.rs", "python call"): (1, (CONVERSION,), "`MetricSnapshot`"),
    ("movement.rs", "python call"): (4, (CONVERSION,), "`Point2D`, `ZoneRoute`"),
    ("population_lifecycle.rs", "python call"): (
        2,
        (ARGUMENT,),
        "the command a Python caller gives, parsed and dumped by its contract"
        " (`PopulationPresenceCommand`, E15)",
    ),
    ("psychology.rs", "python call"): (2, (CONVERSION,), "`AgentStateDelta`, the result"),
    ("service.rs", "python call"): (1, (CONVERSION,), "`ResourceRequestReceipt`"),
    ("social_dialogue.rs", "python call"): (1, (ARGUMENT,), "the items of a payload mapping"),
    ("social_propagation.rs", "python call"): (1, (CONVERSION,), "`AgentStateDelta`"),
    ("toilet.rs", "python call"): (
        2,
        (CONVERSION, HOOK),
        "`ToiletRequestReceipt`; the handler's own `after_complete` (H4)",
    ),
    ("travel.rs", "python call"): (1, (CONVERSION,), "`ZoneRoute`"),
    ("travel_completion.rs", "python call"): (
        2,
        (HOOK,),
        "a manager's own `request` and the controller's `_after_arrival` (H4)",
    ),
    ("world_view.rs", "raw_setattr"): (1, (CONVERSION,), "`object.__new__` and its `__dict__`"),
    ("world_view.rs", "python call"): (
        2,
        (CONVERSION, ARGUMENT),
        "`object.__new__`; the items of a view a Python caller made",
    ),
    ("zone_environment.rs", "python call"): (
        2,
        (CONVERSION,),
        "`ZoneDemandView`, `ZoneEnvironmentView` for a Python reader's entry",
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
    findings: Iterable[Finding],
    exemptions: Mapping[tuple[str, str], tuple[int, tuple[str, ...], str]],
) -> list[str]:
    """What fails: a finding not exempt, a count unlike its exemption's, an exemption unused, and an
    exemption whose class the definition of done does not allow (or E10 outside its files)."""
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
    for (file, what), (_, classes, _) in sorted(exemptions.items()):
        for name in classes or ("none",):
            if name not in CLASSES:
                problems.append(f"{file}: {what} is exempt as {name}, a class not allowed")
            elif name == E10 and file not in E10_FILES:
                problems.append(f"{file}: {what} is exempt as E10 outside {sorted(E10_FILES)}")
    return problems


def judge_exempt_files(exempt_files: Iterable[str]) -> list[str]:
    """An exempt file the definition of done does not allow."""
    return [
        f"{name}: exempt whole, which only {sorted(ALLOWED_EXEMPT_FILES)} may be"
        for name in sorted(set(exempt_files) - ALLOWED_EXEMPT_FILES)
    ]


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
    problems = judge_exempt_files(EXEMPT_FILES)
    problems += judge(scan_rust(rust), EXEMPTIONS)
    problems += scan_python(python)
    problems += scan_stats(native)
    return problems


def main() -> int:
    problems = run()
    for problem in problems:
        print(problem)
    exempt = sum(times for times, _, _ in EXEMPTIONS.values())
    classes = sorted({name for _, names, _ in EXEMPTIONS.values() for name in names})
    print(
        f"glue check: {len(problems)} problems; {len(EXEMPTIONS)} exemptions ({exempt} findings)"
        f" in {len({file for file, _ in EXEMPTIONS})} files, classes {', '.join(classes)};"
        f" {len(EXEMPT_FILES)} exempt files ({', '.join(sorted(EXEMPT_FILES))})"
    )
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

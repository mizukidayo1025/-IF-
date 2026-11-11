"""No code in src/ swaps a WorldState's contents by hand (`__dict__.clear()`, `__dict__.update(`).

A rule changed a copy of the world and then swapped it in with `world.__dict__.clear()` and
`world.__dict__.update(candidate.__dict__)`. Since lane 6-4 of Issue #152 the changes are made on
the world itself in a transaction (`state_mirror.transaction`, policy E5,
docs/rust-port/step5-plan.md), which keeps the world's core, its objects and the rows behind them.
A swap would leave the core holding objects no world has.

The lint finds every call `<x>.__dict__.clear()` and `<x>.__dict__.update(...)` in src/sheltersim,
whatever x is, by the function it is in. It can't see `vars(x).update(...)`,
`dict.update(x.__dict__, ...)`, `x.__dict__ = ...` or writes in native code.
"""

from __future__ import annotations

import ast
from pathlib import Path

ROOT = Path(__file__).parents[3] / "src" / "sheltersim"

# (file, function, method): why this is not a swap of a world. A list that only shrinks: an entry
# the lint no longer finds fails the test below.
ALLOWED: dict[tuple[str, str, str], str] = {
    ("core/state_mirror.py", "transaction_or_copy", "clear"): (
        "the copy-and-swap where the world's core holds no transaction (Unsupported)"
    ),
    ("core/state_mirror.py", "transaction_or_copy", "update"): (
        "the copy-and-swap where the world's core holds no transaction (Unsupported)"
    ),
    ("core/world.py", "_bare_world", "update"): "fills a new world made with object.__new__",
    ("web/pdf_render.py", "save", "update"): "a reportlab canvas's page state, not a world",
}


def scan_source(file: str, source: str) -> list[tuple[str, str, str]]:
    """(file, function, method) of each `<x>.__dict__.clear()`/`.update(` call in source."""
    found: list[tuple[str, str, str]] = []

    def visit(node: ast.AST, function: str) -> None:
        for child in ast.iter_child_nodes(node):
            if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                visit(child, child.name)
                continue
            call = child
            if (
                isinstance(call, ast.Call)
                and isinstance(call.func, ast.Attribute)
                and call.func.attr in {"clear", "update"}
                and isinstance(call.func.value, ast.Attribute)
                and call.func.value.attr == "__dict__"
            ):
                found.append((file, function, call.func.attr))
            visit(child, function)

    visit(ast.parse(source), "<module>")
    return found


def _all_findings() -> list[tuple[str, str, str]]:
    found: list[tuple[str, str, str]] = []
    for path in sorted(ROOT.rglob("*.py")):
        file = path.relative_to(ROOT).as_posix()
        found.extend(scan_source(file, path.read_text(encoding="utf-8")))
    return found


def test_src_swaps_no_world_by_its_dict() -> None:
    new = [item for item in _all_findings() if item not in ALLOWED]

    assert new == [], "change the world in state_mirror.transaction instead of swapping a copy in"


def test_the_list_of_exceptions_only_names_what_the_lint_still_finds() -> None:
    assert sorted(set(ALLOWED) - set(_all_findings())) == []


def test_the_lint_finds_a_swap() -> None:
    source = (
        "def apply(world, candidate):\n"
        "    world.__dict__.clear()\n"
        "    world.__dict__.update(candidate.__dict__)\n"
        "class A:\n"
        "    def f(self):\n"
        "        self.x.__dict__.update({})\n"
    )

    assert scan_source("x.py", source) == [
        ("x.py", "apply", "clear"),
        ("x.py", "apply", "update"),
        ("x.py", "f", "update"),
    ]


def test_the_lint_accepts_other_dicts() -> None:
    source = "def f(world, d):\n    d.update({})\n    world.__dict__.get('_core')\n    d.clear()\n"

    assert scan_source("x.py", source) == []

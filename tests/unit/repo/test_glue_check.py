"""The glue check of Issue #152 (benchmarks/rust_port/glue_check.py, design-p9.md section 6).

The PyO3 crate's rules files may hold only glue: what the deny list names, the calls into Python
and the `with_world(` blocks each stay only with an exemption that says why, with its count. The
check is clean on this tree, and it fails on a finding it does not know, on an exemption whose count
is stale, on a deleted Python fallback coming back, and on a moved step that has a body again.
"""

from __future__ import annotations

import importlib.util
import sys
from pathlib import Path

ROOT = Path(__file__).parents[3]


def _glue_check():
    spec = importlib.util.spec_from_file_location(
        "glue_check", ROOT / "benchmarks" / "rust_port" / "glue_check.py"
    )
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    sys.modules.setdefault(spec.name, module)
    spec.loader.exec_module(module)
    return module


def test_the_tree_is_glue_or_says_why() -> None:
    check = _glue_check()
    assert check.run(ROOT) == []
    assert all(what.strip() for _, _, what in check.EXEMPTIONS.values())
    assert all(times > 0 for times, _, _ in check.EXEMPTIONS.values())
    for _, classes, _ in check.EXEMPTIONS.values():
        assert classes and set(classes) <= set(check.CLASSES)
    assert set(check.EXEMPT_FILES) <= check.ALLOWED_EXEMPT_FILES


def test_a_finding_without_an_exemption_and_a_stale_exemption_fail() -> None:
    check = _glue_check()
    files = {
        "probe.rs": (
            "use crate::core::world_core::namespace_held;\n"
            "// namespace_held in a comment does not count\n"
            "fn read(world: &Bound<'_, PyAny>) -> PyResult<Held> {\n"
            '    let held = namespace_held(world, key, "other")?;\n'
            "    let total = py_fsum(&values);\n"
            "    with_world(world, |cell| core::one(cell))?;\n"
            "    with_world(world, |cell| { let a = core::one(cell); core::two(cell, a) })?;\n"
            "    class.call1((1,))\n"
            "}\n"
        ),
        "parity.rs": 'fn compare() { namespace_held(world, key, "x"); }\n',
    }
    found = {(finding.file, finding.what): finding.lines for finding in check.scan_rust(files)}
    assert found == {
        ("probe.rs", "namespace_held"): (4,),
        ("probe.rs", "py_fsum"): (5,),
        ("probe.rs", "python call"): (8,),
        ("probe.rs", check.CLOSURE): (7,),
    }
    exemptions = {
        ("probe.rs", "namespace_held"): (1, (check.ARGUMENT,), "a reason"),
        ("probe.rs", "py_fsum"): (2, (check.ARGUMENT,), "a reason"),
        ("probe.rs", "python call"): (1, (check.CONVERSION,), "a reason"),
        ("gone.rs", "EventWriter"): (1, (check.HOOK,), "a reason"),
    }
    problems = check.judge(check.scan_rust(files), exemptions)
    assert len(problems) == 3
    assert any("py_fsum x1" in problem and "says 2" in problem for problem in problems)
    assert any(check.CLOSURE in problem and "not allowed" in problem for problem in problems)
    assert any(problem.startswith("gone.rs: EventWriter") for problem in problems)


def test_an_exemption_of_a_class_not_allowed_and_an_exempt_file_not_allowed_fail() -> None:
    check = _glue_check()
    files = {
        "probe.rs": "fn read(world: W) { class.call1((1,)); EventWriter::of(world); }\n",
        "tick.rs": "fn accept(world: W) { EventWriter::of(world); }\n",
    }
    exemptions = {
        ("probe.rs", "python call"): (1, (check.CONVERSION,), "a class for a Python caller"),
        ("probe.rs", "EventWriter"): (1, (check.E10,), "E10 outside tick.rs and dispatch.rs"),
        ("tick.rs", "EventWriter"): (1, (check.E10,), "the tick's check"),
    }
    problems = check.judge(check.scan_rust(files), exemptions)
    assert problems == ["probe.rs: EventWriter is exempt as E10 outside ['dispatch.rs', 'tick.rs']"]
    queue = {("probe.rs", "EventWriter"): (1, ("QUEUE",), "the queue's objects")}
    problems = check.judge(check.scan_rust({"probe.rs": files["probe.rs"]}), queue)
    assert "probe.rs: EventWriter is exempt as QUEUE, a class not allowed" in problems
    assert check.judge_exempt_files(["parity.rs", "after_tick.rs"]) == []
    assert check.judge_exempt_files(["movement_geometry.rs"]) == [
        "movement_geometry.rs: exempt whole, which only"
        " ['after_tick.rs', 'parity.rs', 'tick_validation.rs'] may be"
    ]


def test_a_deleted_python_rule_and_a_step_with_a_body_fail() -> None:
    check = _glue_check()
    sources = {
        "domain/care_receipts.py": "def care_source_ids_for_agent(world, i):\n    return ()\n",
        "domain/other.py": "from x import make_care_transition_record\n",
        "experiments/phases/dispatch.py": (
            "class DispatchPhase:\n"
            "    @kernel_only('dispatch', 'sync_movement')\n"
            "    def _sync_movement_after_events(self):\n"
            "        '''Finish the journeys.'''\n"
            "    @kernel_only('dispatch', 'consider_contact')\n"
            "    def _consider_social_contact_event(self, event):\n"
            "        '''Ask.'''\n"
            "        return None\n"
        ),
    }
    problems = check.scan_python(sources)
    assert any("care_source_ids_for_agent" in problem for problem in problems)
    assert any("make_care_transition_record" in problem for problem in problems)
    assert any("_consider_social_contact_event must be" in problem for problem in problems)
    assert not any("_sync_movement_after_events" in problem for problem in problems)
    assert check.scan_stats({"src/rules/dispatch.rs": 'counts.set_item("events_python", 1)'})

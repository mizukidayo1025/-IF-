"""The new screen's scene trims a person's walking path between two points without recursing
forever on a path that turns back over itself."""

import shutil
import subprocess
from pathlib import Path

import pytest


def test_a_path_that_turns_back_over_itself_is_trimmed_without_endless_recursion():
    """Replay stopped with `RangeError: Maximum call stack size exceeded` in a 7-day Run
    (minutes 5119→5120 and 5160→5161 of the precomputed sample): on a path that goes out
    and back, the start projects after the end both ways round, and trimPath reversed the
    path again and again."""
    node = shutil.which("node")
    if node is None:
        pytest.skip("Node is required for the scene regression")
    source = Path("src/sheltersim/web/static/demo-v2-scene.js").resolve()
    script = r"""
import fs from "node:fs";
import assert from "node:assert/strict";
const source = fs.readFileSync(process.argv[1], "utf8");
const pick = (start, end) => start + source.split(start)[1].split(end)[0];
const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const distance = (a, b) => Math.hypot(a.x - b.x, a.y - b.y);
const progressAt = eval(
  "(" + pick("function progressAt(points, p) {", "  function trimPath(") + ")",
);
const trimPath = eval(
  "(" + pick("function trimPath(path, start, end", "  function joinPaths(") + ")",
);
const p = (x, y) => ({ x, y });
// Out to (2,0) and back: (1.5,0) projects before (0.5,0) on the way out both ways round.
const outAndBack = [p(0, 0), p(2, 0), p(0, 0)];
assert.equal(trimPath(outAndBack, p(1.5, 0), p(0.5, 0)), null);
// A path walked the other way is reversed once and trimmed.
const straight = [p(0, 0), p(1, 0), p(2, 0), p(3, 0)];
assert.deepEqual(trimPath(straight, p(2.5, 0), p(0.5, 0)), [
  p(2.5, 0), p(2, 0), p(1, 0), p(0.5, 0),
]);
// The usual direction keeps its inner points, and a point off the path gives none.
assert.deepEqual(trimPath(straight, p(0.5, 0), p(2.5, 0)), [
  p(0.5, 0), p(1, 0), p(2, 0), p(2.5, 0),
]);
assert.equal(trimPath(straight, p(0.5, 1), p(2.5, 0)), null);
"""
    subprocess.run(
        [node, "--input-type=module", "-e", script, str(source)],
        check=True,
        capture_output=True,
        text=True,
    )

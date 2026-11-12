"""`pytest --shard I/N` keeps part I of N of the selected tests (tests/conftest.py, #118).

The offline CI runs each OS's tests in two jobs, one part each: the parts must not overlap,
must together be every selected test, and must be taken after -m deselects.
"""

from __future__ import annotations

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).parents[3]
# A directory of quick tests, collected in a few seconds.
TARGET = "tests/unit/repo"


def _collect(*args: str) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [sys.executable, "-m", "pytest", "--collect-only", "-q", "-p", "no:cacheprovider"]
        + [TARGET, *args],
        cwd=ROOT,
        env={**os.environ, "SHELTERSIM_LLM_MODE": "offline"},
        capture_output=True,
        text=True,
        timeout=300,
    )


def _ids(*args: str) -> list[str]:
    result = _collect(*args)
    assert result.returncode == 0, result.stdout + result.stderr
    return [line for line in result.stdout.splitlines() if "::" in line]


def test_the_parts_take_the_selected_tests_in_turn() -> None:
    every = _ids()
    first = _ids("--shard", "1/2")
    second = _ids("--shard", "2/2")
    assert len(every) > 10
    assert first == every[0::2]
    assert second == every[1::2]
    assert _ids("--shard", "1/1") == every


def test_the_parts_share_what_a_mark_left() -> None:
    selected = _ids("-m", "not long_simulation")
    parts = [_ids("-m", "not long_simulation", "--shard", f"{index}/3") for index in (1, 2, 3)]
    assert sorted(sum(parts, [])) == sorted(selected)
    assert max(map(len, parts)) - min(map(len, parts)) <= 1


def test_a_shard_outside_its_count_is_refused() -> None:
    for value in ("0/2", "3/2", "2", "a/b"):
        result = _collect("--shard", value)
        assert result.returncode == 4, value
        assert "--shard takes I/N" in result.stderr, value

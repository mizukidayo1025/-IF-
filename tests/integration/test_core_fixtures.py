"""The fixtures of the core's tests are what the Python side makes today (Issue #225).

`native/core/tests/` (cargo test) reads worlds the Python side built, with the values the native
path gives on them, from the files `support/core_fixtures.py` registers (`FIXTURES`). Each file
must be what its builder makes now: a change of a world, of the exporter or of the native path's
values shows up here, and `benchmarks/core_fixtures.py` writes the files again.
"""

from __future__ import annotations

import json
from typing import Any

import pytest
from support.core_fixtures import FIXTURES, build, fixture_path, render


def _first_difference(committed: Any, made: Any, path: str = "") -> str:
    """The path of the first value that differs, for a short failure message."""
    if isinstance(committed, dict) and isinstance(made, dict):
        for key in sorted(set(committed) | set(made)):
            if committed.get(key, ...) != made.get(key, ...):
                return _first_difference(committed.get(key), made.get(key), f"{path}/{key}")
    elif isinstance(committed, list) and isinstance(made, list) and len(committed) == len(made):
        for index, (left, right) in enumerate(zip(committed, made)):
            if left != right:
                return _first_difference(left, right, f"{path}/{index}")
    return path or "/"


@pytest.mark.parametrize("name", sorted(FIXTURES))
def test_a_core_fixture_is_what_its_builder_makes_today(name: str) -> None:
    made = render(build(name))
    committed = fixture_path(name).read_text(encoding="utf-8")
    if committed != made:
        where = _first_difference(json.loads(committed), json.loads(made))
        pytest.fail(
            f"native/core/tests/data/{name}.json differs at {where}; write it again with "
            f"`python benchmarks/core_fixtures.py {name}` if the change is meant"
        )


def test_the_registered_files_are_the_fixtures_under_the_core_s_data() -> None:
    """Each registered file exists, and each file of worlds under the data is registered."""
    data = fixture_path("x").parent
    for name in FIXTURES:
        assert fixture_path(name).is_file(), name
    with_worlds = {
        path.stem
        for path in data.glob("*.json")
        if "worlds" in json.loads(path.read_text(encoding="utf-8"))
    }
    assert with_worlds <= set(FIXTURES)

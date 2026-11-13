"""The scenario corpus is what the Python server answers today (Issue #293).

`native/core/tests/data/scenario_corpus.json` holds, for each case of
`support/scenario_corpus.py`, the body the Scenario screen posts to `scenario-preview` and what
the server makes of it offline, with the clock and the public services pinned: the browser's
port (#291) is checked against it. Each case is made again here and must equal the committed one;
making it also checks that its Scenario is a fixed point of `ScenarioSpec.model_validate`, since
the browser builds Scenarios without the full validation (#285). Write the file again with
`python benchmarks/scenario_corpus.py [CASE ...]` when a change is meant.
"""

from __future__ import annotations

import json
from typing import Any

import pytest
from support.scenario_corpus import CORPUS_PATH, case_names, case_outputs, expand, expanded_case


def _committed() -> dict[str, Any]:
    return json.loads(CORPUS_PATH.read_text(encoding="utf-8"))


def _first_difference(committed: Any, made: Any, path: str = "") -> str:
    if isinstance(committed, dict) and isinstance(made, dict):
        for key in sorted(set(committed) | set(made)):
            if committed.get(key, ...) != made.get(key, ...):
                return _first_difference(committed.get(key), made.get(key), f"{path}/{key}")
    elif isinstance(committed, list) and isinstance(made, list) and len(committed) == len(made):
        for index, (left, right) in enumerate(zip(committed, made)):
            if left != right:
                return _first_difference(left, right, f"{path}/{index}")
    return path or "/"


@pytest.mark.parametrize("name", case_names())
def test_a_corpus_case_is_what_the_server_answers_today(name: str) -> None:
    document = _committed()
    committed = expand(document).get(name)
    assert committed is not None, f"{name} is not in the corpus; write it again"
    made, research = expanded_case(case_outputs(name))
    if research is not None:
        committed = {**committed, "research_document": document["research"][committed["research"]]}
        made = {**made, "research_document": research}
    if committed != made:
        pytest.fail(
            f"case {name} differs at {_first_difference(committed, made)}; write it again with "
            f"`python benchmarks/scenario_corpus.py {name}` if the change is meant"
        )


def test_the_corpus_holds_every_case_and_only_its_research() -> None:
    document = _committed()
    assert sorted(document["cases"]) == sorted(case_names())
    used = {case["research"] for case in document["cases"].values() if "research" in case}
    assert set(document["research"]) == used

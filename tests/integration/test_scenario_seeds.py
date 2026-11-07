"""A Scenario's seed is any int from 0, past 64 bits too.

`CounterRNG` keys its draws with `str(seed)`, and the arrivals of `add_evacuees` take their IDs
from `f"{world.seed}..."`. The kernels read such a seed as its text; before, some of them read it
as an i64 and refused, and the Python rule ran instead. These Runs of the gate's Scenarios (the
gate's builders, with only the seed changed) give the results that the Python rules gave, recorded
once from the tree before their bodies were deleted (87b6f10), and no kernel refuses on the way.
"""

from __future__ import annotations

import hashlib
import json
import os
import sys
from pathlib import Path
from typing import Any

import pytest
from support.paths import BENCHMARKS

# The gate Scenarios that draw: toilets in all of them, behavior, information, social propagation
# and proximity. The first has the generated Scenario's own models (sleep-aware-utility-softmax-v2
# and evidence-aware-rumor-v1).
SCENARIOS = (
    "model__behavior__sleep-aware-utility-softmax-v2",
    "model__behavior__utility-softmax-v1",
    "model__information__hazard-information-v1",
    "model__information__sparse-information-contact-v1",
    "model__social__degroot-contact-v1",
    "model__social_proximity__distance-sampled-proximity-v1",
)
# The arrivals and departures of the population Scenario: a test of its own, a long one.
SEEDS = (2**64 + 7, 2**63, 10**30)

# The first 16 hex digits of the SHA-256 of each Scenario's `{"refused": ..., "runs": ...}`
# (`_digest`), from the Python rules. Pinned on Linux only: other systems have other math libraries.
PYTHON_RULES_DIGESTS: dict[int, dict[str, str]] = {
    2**64 + 7: {
        "model__behavior__sleep-aware-utility-softmax-v2": "3c26dda0a427e0d3",
        "model__behavior__utility-softmax-v1": "a30f9ee8539e54e2",
        "model__information__hazard-information-v1": "3c9d11adf3fa840c",
        "model__information__sparse-information-contact-v1": "bff8c84e1bcc2724",
        "model__social__degroot-contact-v1": "4fb77f80a30cfc45",
        "model__social_proximity__distance-sampled-proximity-v1": "8d50bf62d9fc88eb",
        "population": "f62370281c01fd25",
    },
    2**63: {
        "model__behavior__sleep-aware-utility-softmax-v2": "5dcea19b5db4257d",
        "model__behavior__utility-softmax-v1": "2a3bafb9d8c05e6b",
        "model__information__hazard-information-v1": "1142d3314c3cb821",
        "model__information__sparse-information-contact-v1": "ccbb716cd9c3e791",
        "model__social__degroot-contact-v1": "8704af90a88467cd",
        "model__social_proximity__distance-sampled-proximity-v1": "fd3a3fee17ab96ff",
        "population": "e89b8fc025064fbf",
    },
    10**30: {
        "model__behavior__sleep-aware-utility-softmax-v2": "e8d9ec886ff5f142",
        "model__behavior__utility-softmax-v1": "3ed95249240dc8c7",
        "model__information__hazard-information-v1": "2eaa11e50f3f1dbb",
        "model__information__sparse-information-contact-v1": "35e667379718cb68",
        "model__social__degroot-contact-v1": "400c021fb1c0f2e7",
        "model__social_proximity__distance-sampled-proximity-v1": "79bfaa8331eb53ae",
        "population": "a633818c39c4819a",
    },
}


def _digest(outcome: Any) -> str:
    document = {"refused": outcome.refused, "runs": outcome.runs}
    text = json.dumps(document, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def _gate_scenarios() -> Any:
    sys.path.insert(0, str(BENCHMARKS))
    try:
        import gate_scenarios
    finally:
        sys.path.remove(str(BENCHMARKS))
    return gate_scenarios


def _run(seed: int, names: tuple[str, ...], directory: Path) -> tuple[dict[str, str], list]:
    """Each gate Scenario's digest with this seed, and the refusals the kernels raised."""
    import sheltersim_native

    gate = _gate_scenarios()
    with pytest.MonkeyPatch.context() as patch:
        patch.setitem(gate.generated.__kwdefaults__, "seed", seed)
        scenarios = gate.all_scenarios()
        # Under a census of the tests, its switch is not this test's to turn.
        counting = "SHELTERSIM_REFUSAL_CENSUS" not in os.environ
        if counting:
            sheltersim_native.census_take()
            sheltersim_native.census_switch(True)
        try:
            digests = {name: _digest(scenarios[name].run(directory / name)) for name in names}
            raised, _ = sheltersim_native.census_take() if counting else ([], [])
        finally:
            if counting:
                sheltersim_native.census_switch(False)
    return digests, raised


@pytest.mark.parametrize("seed", SEEDS)
def test_a_seed_past_64_bits_gives_the_python_rules_results_and_no_kernel_refuses(
    seed: int, tmp_path: Path
) -> None:
    digests, raised = _run(seed, SCENARIOS, tmp_path)
    assert raised == []
    if sys.platform == "linux":
        assert digests == {name: PYTHON_RULES_DIGESTS[seed][name] for name in SCENARIOS}


@pytest.mark.long_simulation
def test_arrivals_and_departures_with_a_seed_past_64_bits_give_the_python_rules_results(
    tmp_path: Path,
) -> None:
    """The gate's population Scenario, ten Branches, for each seed: over 30 seconds."""
    for seed in SEEDS:
        digests, raised = _run(seed, ("population",), tmp_path / str(seed))
        assert raised == []
        if sys.platform == "linux":
            assert digests == {"population": PYTHON_RULES_DIGESTS[seed]["population"]}

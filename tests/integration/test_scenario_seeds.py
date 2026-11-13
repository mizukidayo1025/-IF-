"""A Scenario's seed is any int from 0, past 64 bits too.

`CounterRNG` keys its draws with `str(seed)`, and the arrivals of `add_evacuees` take their IDs
from `f"{world.seed}..."`. The kernels read such a seed as its text; before, some of them read it
as an i64 and refused, and the Python rule ran instead. These Runs of the gate's Scenarios (the
gate's builders, with only the seed changed) give the results that the Python rules gave, recorded
once from the tree before their bodies were deleted (87b6f10), and no kernel refuses on the way.

Model version 2 (step 4, L11: the pinned libm crate and the information events' draw keys)
changed some of these results: those digests were re-recorded from the kernels of that model
and are no longer the deleted Python rules'.
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
# (`_digest`), from the Python rules. The same on every OS, as the gate's hashes.
PYTHON_RULES_DIGESTS: dict[int, dict[str, str]] = {
    2**64 + 7: {
        "model__behavior__sleep-aware-utility-softmax-v2": "0e7c9c9112f7fd75",
        "model__behavior__utility-softmax-v1": "57371f5148fe4c02",
        "model__information__hazard-information-v1": "f0f7b0be6edbb6df",
        "model__information__sparse-information-contact-v1": "97bf3952557145c7",
        "model__social__degroot-contact-v1": "31de527f033a2aac",
        "model__social_proximity__distance-sampled-proximity-v1": "dabcf6f06f8f4247",
        "population": "4aac2db96cfd8524",
    },
    2**63: {
        "model__behavior__sleep-aware-utility-softmax-v2": "47a0af77bfd05036",
        "model__behavior__utility-softmax-v1": "23eb2998b8475cca",
        "model__information__hazard-information-v1": "7b6fc88b2066c103",
        "model__information__sparse-information-contact-v1": "fc16811903d1c2d6",
        "model__social__degroot-contact-v1": "333c7ca620609ef8",
        "model__social_proximity__distance-sampled-proximity-v1": "32e06fe2998d1590",
        "population": "87987ab675e7ed24",
    },
    10**30: {
        "model__behavior__sleep-aware-utility-softmax-v2": "ad6c0d345dc35ed4",
        "model__behavior__utility-softmax-v1": "06adeb1ec493132c",
        "model__information__hazard-information-v1": "78ae4b6db0535324",
        "model__information__sparse-information-contact-v1": "f6bd07faea361064",
        "model__social__degroot-contact-v1": "e46ad8258bbc4f24",
        "model__social_proximity__distance-sampled-proximity-v1": "2d1bee77c1184c17",
        "population": "1f878c257b36dac1",
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
    assert digests == {name: PYTHON_RULES_DIGESTS[seed][name] for name in SCENARIOS}


@pytest.mark.long_simulation
def test_arrivals_and_departures_with_a_seed_past_64_bits_give_the_python_rules_results(
    tmp_path: Path,
) -> None:
    """The gate's population Scenario, ten Branches, for each seed: over 30 seconds."""
    for seed in SEEDS:
        digests, raised = _run(seed, ("population",), tmp_path / str(seed))
        assert raised == []
        assert digests == {"population": PYTHON_RULES_DIGESTS[seed]["population"]}

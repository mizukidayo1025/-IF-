"""The frame of a minute is the core's, the frame Python made before (#264).

`inspection.frames.world_frame` reads the frame the core makes (`sheltersim_core::frame`, through
the native rule `frame.world_frame`). Until PR #270 Python made it; these frames have the sha256
of their canonical JSON that the Python frame had then, on the same worlds (generated Scenarios of
20 and 100 people over a day, at 0, 1, 720 and 1439 minutes). The worlds of
`support/core_frames.py` (no floor plan, facilities moved by an EffectProgram, people walking in
on an approach route) are checked the same way by `native/core/tests/data/frames.json`, which
`integration/test_core_fixtures.py` rebuilds.
"""

from __future__ import annotations

import hashlib

import pytest
from support.core_fixtures import run_world
from support.core_tick import generated_run

from sheltersim.core.checkpoint_cache import canonical_bytes
from sheltersim.inspection.frames import world_frame, world_frame_bytes

# The sha256 of the Python frame at each minute, by the number of people (PR #270).
PYTHON_FRAMES: dict[int, tuple[tuple[int, str], ...]] = {
    20: (
        (0, "bea6f586cc8ad250f7b7a2425fc357e801a095132c770ea98726c92a5dfd52b1"),
        (1, "1397479e46fcdd3e0747bdcd831cc22fa324a3a3b6c4355a06d0a6fbf6e8b37c"),
        (720, "fd0b008077a7f543ec55e40e18f23b7ab88cb7c7f2b6b1308a89c245d548fe67"),
        (1439, "83fec79e246a6bfbd64af98941f51140d608705bba518a2637d95ff32401d3d7"),
    ),
    100: (
        (0, "a4ac5bc38d64a96b2d8ff70eac36bf125075f26832cafb12fa6efa0cd40125be"),
        (1, "0653b17e26c171760efe665a27c81cedb8be85414a66152b5cc55948360cf6e3"),
        (720, "f2878c89fb3d5efa7ba899fe73891ac7b862c6cb21429bc13db96e399e3b9412"),
        (1439, "76f2054e7c1fe542c52bd4c5cc9d71eed6d8b8769a133b667a8b18fd2f9a864f"),
    ),
}


def _generated(people: int):
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )

    request = InitialScenarioRequest(
        municipality="合成市", disaster="地震", evacuees=people, duration_days=1, seed=2026
    )
    return run_world(InitialScenarioBuilder().build(request).scenario)


@pytest.mark.parametrize("people", sorted(PYTHON_FRAMES))
def test_frame_of_a_generated_day_is_the_one_python_made(people: int) -> None:
    world, _, engine = _generated(people)
    for minute, digest in PYTHON_FRAMES[people]:
        while world.minute_index < minute:
            engine.step()
        raw = world_frame_bytes(world, run_id="run", branch_id="main", computed_until=minute)
        assert hashlib.sha256(raw).hexdigest() == digest, minute
    frame = world_frame(world, run_id="run", branch_id="main", computed_until=1439)
    assert canonical_bytes(frame) == raw
    assert any(person["care_history"] for person in frame["people"])
    if people == 100:
        assert frame["recorded_history"]["social"]["event_window_truncated"]


def test_frame_names_the_digest_given_and_changes_nothing() -> None:
    world, _, engine = generated_run()
    for _ in range(40):
        engine.step()
    before = world.canonical_json()
    framed = world_frame(
        world, run_id="run", branch_id="main", computed_until=60, state_hash="0" * 64
    )
    assert framed["state_hash"] == "0" * 64
    assert (framed["minute"], framed["computed_until"]) == (40, 60)
    framed = world_frame(world, run_id="run", branch_id="main", computed_until=40)
    assert framed["state_hash"] == world.state_digest()
    assert world.canonical_json() == before
    engine.step()

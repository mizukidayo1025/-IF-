"""The core's frame of a minute is the Python frame, byte for byte (#264).

`sheltersim_core::frame` (through the native rule `frame.world_frame`) makes, from the world's core,
the frame `inspection.frames.world_frame` makes in Python: compared as canonical JSON, the same
keys, values, ints and floats, with the same `state_hash` (`world.state_digest()`) where the
caller gives none. The worlds are run through `TickEngine`: generated Scenarios of 20 and 100
people over a day, and the worlds of `support/core_frames.py` (no floor plan, facilities moved by
an EffectProgram, people walking in on an approach route).
"""

from __future__ import annotations

import json

import pytest
from support.core_fixtures import run_world
from support.core_frames import MINUTES, arrivals_run
from support.core_tick import core_tick_run, generated_run

from sheltersim import native
from sheltersim.core.checkpoint_cache import canonical_bytes
from sheltersim.inspection.frames import world_frame


def _core_frame(world, **request) -> bytes:
    return bytes(native.rules("frame").world_frame(world, **request))


def _same_frames(world, **request) -> dict:
    """The Python frame, after checking that the core's is the same bytes."""
    python = world_frame(world, **request)
    assert _core_frame(world, **request) == canonical_bytes(python)
    return python


def _generated(people: int):
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )

    request = InitialScenarioRequest(
        municipality="合成市", disaster="地震", evacuees=people, duration_days=1, seed=2026
    )
    return run_world(InitialScenarioBuilder().build(request).scenario)


@pytest.mark.parametrize("people", [20, 100])
def test_core_frame_is_the_python_frame_of_a_generated_day(people: int) -> None:
    world, _, engine = _generated(people)
    frames = []
    for minute in (0, 1, 720, 1439):
        while world.minute_index < minute:
            engine.step()
        frames.append(_same_frames(world, run_id="run", branch_id="main", computed_until=minute))
    last = frames[-1]
    assert any(person["care_history"] for person in last["people"])
    if people == 100:
        assert last["recorded_history"]["social"]["event_window_truncated"]


@pytest.mark.parametrize(
    ("name", "run"),
    [("small_replay", core_tick_run), ("generated", generated_run), ("arrivals", arrivals_run)],
)
def test_core_frame_is_the_python_frame_of_the_fixture_worlds(name: str, run) -> None:
    world, _, engine = run()
    for minute in MINUTES[name]:
        while world.minute_index < minute:
            engine.step()
        _same_frames(world, run_id="run", branch_id="main", computed_until=minute)


def test_core_frame_names_the_digest_given_and_changes_nothing() -> None:
    world, _, engine = generated_run()
    for _ in range(40):
        engine.step()
    before = world.canonical_json()
    digest = world.state_digest()
    framed = json.loads(
        _core_frame(world, run_id="run", branch_id="main", computed_until=60, state_hash="0" * 64)
    )
    assert framed["state_hash"] == "0" * 64
    assert (framed["minute"], framed["computed_until"]) == (40, 60)
    assert (
        json.loads(_core_frame(world, run_id="run", branch_id="main", computed_until=40))[
            "state_hash"
        ]
        == digest
    )
    assert world.canonical_json() == before
    engine.step()

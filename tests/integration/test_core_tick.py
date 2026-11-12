"""The core's tick without Python against the native path (Issue #152, P10-1b).

`native/core/tests/tick_core.rs` (cargo test) builds the World of
`native/core/tests/data/tick_core.json` without Python and runs the core's `run_tick` for
1,440 ticks with a test host; it must reach the digests the fixture freezes. These tests
check the other side: the fixture is still the world `tests/support/core_tick.py` builds, and
the same world through `TickEngine` (the native path, offline) still gives those digests. So
both paths give the same bytes of every part of the world, the same event log and the same
metric snapshots. `benchmarks/core_tick_fixture.py` writes the fixture again after a change
that moves them.
"""

from __future__ import annotations

import json

from support.core_tick import TICKS, core_tick_run, fixture_document, native_result
from support.paths import CORE_TICK_FIXTURE


def _fixture() -> dict:
    return json.loads(CORE_TICK_FIXTURE.read_text(encoding="utf-8"))


def test_the_fixture_is_the_small_world_at_minute_zero() -> None:
    fixture = _fixture()
    world, controller, _ = core_tick_run()
    document = json.loads(json.dumps(fixture_document(world, controller), sort_keys=True))
    assert {key: fixture[key] for key in document} == document


def test_the_native_path_gives_the_digests_the_core_reaches_alone() -> None:
    expected = _fixture()["expected"]
    assert expected["ticks"] == TICKS
    result = native_result()
    assert result["event_types"] == expected["event_types"]
    assert result["metric_count"] == expected["metric_count"] == TICKS
    assert result["world"] == expected["world"]
    assert result["metrics"] == expected["metrics"]
    # The run is not empty: the toilet's outage and recovery, toilets, staff support and its
    # timeouts all happen in it.
    assert {
        "facility_outage",
        "facility_recovery",
        "toilet_service_complete",
        "staff_support_complete",
        "support_request_timeout",
        "agent_travel_complete",
        "agent_action_lifecycle",
    } <= set(result["event_types"])

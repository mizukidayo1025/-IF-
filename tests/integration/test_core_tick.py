"""The core's tick without Python against the native path (Issue #152, P10-1b).

`native/core/tests/tick_core.rs` (cargo test) builds the World of
`native/core/tests/data/tick_core.json` without Python and runs the core's `run_tick` for
1,440 ticks with a test host; it must reach the digests the fixture freezes. The fixture is the
world `support/core_tick.py` builds and the digests the same world gives through `TickEngine`
(the native path, offline): `integration/test_core_fixtures.py` checks that the file is what they
are today, so both paths give the same bytes of every part of the world, the same event log and
the same metric snapshots. This file checks that the run the fixture freezes is not empty.
"""

from __future__ import annotations

import json

from support.core_fixtures import fixture_path
from support.core_tick import TICKS


def test_the_fixture_s_run_has_the_events_the_core_must_handle() -> None:
    entry = json.loads(fixture_path("tick_core").read_text(encoding="utf-8"))["worlds"]
    expected = entry["small_replay"]["expected"]
    assert expected["ticks"] == TICKS
    assert expected["metric_count"] == TICKS
    # The toilet's outage and recovery, toilets, staff support and its timeouts all happen in it.
    assert {
        "facility_outage",
        "facility_recovery",
        "toilet_service_complete",
        "staff_support_complete",
        "support_request_timeout",
        "agent_travel_complete",
        "agent_action_lifecycle",
    } <= set(expected["event_types"])

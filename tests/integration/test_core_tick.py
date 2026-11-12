"""The core's tick without Python against the native path (Issue #152, P10-1b; #214).

`native/core/tests/tick_core.rs` (cargo test) builds each World of
`native/core/tests/data/tick_core.json` without Python and runs the core's `run_tick` for the
world's ticks with a test host; it must reach the digests the fixture freezes. The fixture is the
worlds `support/core_tick.py` builds and the digests the same worlds give through `TickEngine`
(the native path, offline): `integration/test_core_fixtures.py` checks that the file is what they
are today, so both paths give the same bytes of every part of the world, the same event log and
the same metric snapshots. This file checks that the runs the fixture freezes are not empty.
"""

from __future__ import annotations

import json
from typing import Any

from support.core_fixtures import fixture_path
from support.core_tick import FACILITY_MOVES, GENERATED_TICKS, TICKS


def _worlds() -> dict[str, Any]:
    return json.loads(fixture_path("tick_core").read_text(encoding="utf-8"))["worlds"]


def test_the_fixture_s_run_has_the_events_the_core_must_handle() -> None:
    expected = _worlds()["small_replay"]["expected"]
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


def test_people_walk_and_replan_in_the_generated_world() -> None:
    entry = _worlds()["generated"]
    expected = entry["expected"]
    assert expected["ticks"] == GENERATED_TICKS
    assert expected["metric_count"] == GENERATED_TICKS
    # The extensions of a floor plan, with independent queues, and the typed namespaces.
    assert {
        "agent_dynamics_v1",
        "population_dynamics_v1",
        "social_network_v1",
        "staff_dynamics_v1",
        "staff_operations_v1",
        "movement_v1",
        "facility_queues_v1",
        "effect_program_v1",
    } <= set(entry["world"]["model_state"])
    # People are on trips through many ticks, and each move replans a trip under way.
    assert expected["ticks_with_journeys"] >= 60
    assert expected["replanned"] >= len(FACILITY_MOVES)
    assert expected["event_types"].count("effect_program_geometry") == len(FACILITY_MOVES)
    assert {
        "facility_queue_transition",
        "agent_travel_complete",
        "toilet_service_complete",
        "staff_support_complete",
        "resource_service_complete",
    } <= set(expected["event_types"])

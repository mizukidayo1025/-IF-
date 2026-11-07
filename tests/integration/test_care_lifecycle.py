"""A generated Scenario keeps every care transition observation over a whole day, and an event
that validates the world in the minute a support request times out sees a valid world.

The care receipts themselves are checked in tests/unit/domain/test_care_receipts.py.
"""

from __future__ import annotations

import copy
from pathlib import Path
from types import SimpleNamespace

import pytest

from sheltersim.core.engine import TraceLevel
from sheltersim.core.world import StateInvariantError
from sheltersim.domain.care_receipts import (
    care_history_by_agent,
    care_receipt_for_request,
    ingest_applied_shared_operation_records,
)
from sheltersim.events.effects import (
    DEFAULT_PRIMITIVE_REGISTRY,
    EffectProgram,
    SupportMode,
    TargetSelector,
    TimedAction,
)
from sheltersim.events.models import EventStatus
from sheltersim.events.queue import EventLog
from sheltersim.experiments.runner import OfflineSimulationController, run_scenario
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.storage.projects import digest


def test_full_day_shared_care_receipts_keep_each_transition_observation(
    generated_previews, tmp_path: Path
) -> None:
    preview = generated_previews(
        evacuees=8,
        duration_days=1,
        gross_area_m2=1200,
        accessible_toilet_count=2,
        staff_count=2,
        evacuation_selection={"age_band_multipliers": {"child": 0, "adult": 1, "older_adult": 0}},
    )

    assert preview["can_start"]
    scenario = ScenarioSpec.model_validate(copy.deepcopy(preview["scenario"]))
    world = run_scenario(scenario, tmp_path / "one-day-care", trace_level=TraceLevel.OFF)

    assert world.minute_index == 1440
    world.validate()

    # The rows a frame reuses as the EventLog grows are the rows of a full pass.
    records = world.event_log.records_from(0)
    grown = SimpleNamespace(event_log=EventLog())
    for end in range(0, len(records) + 500, 500):
        for record in records[len(grown.event_log) : end]:
            grown.event_log.append(record)
        full_pass = SimpleNamespace(event_log=EventLog(records[: len(grown.event_log)]))
        assert care_history_by_agent(grown) == care_history_by_agent(full_pass)
    assert care_history_by_agent(grown)


# A support request that times out closes inside the batch of due events, and the dispatch phase
# records its care interruption after the batch. An event later in the same batch whose handler
# validates the world (a closure's expiry copies the world) used to stop the Run (#88, 元 #309).
START = 30


class _NoArtifacts:
    trace_level = TraceLevel.OFF

    def record_decision(self, *_args: object, **_kwargs: object) -> None:
        pass

    def capture_metric(self, *_args: object, **_kwargs: object) -> None:
        pass


@pytest.fixture(scope="module")
def closure_at_a_timeout(tmp_path_factory, generated_previews, saved_timeline):
    """A Branch whose corridor-toilet closure expires in the minute support requests time out.

    Returns the timeline, the Branch and that minute. The timeouts were scheduled before the
    closure, so they come first in that minute's batch.
    """
    scenario = ScenarioSpec.model_validate(
        copy.deepcopy(generated_previews(evacuees=30, duration_days=180 / 1440)["scenario"])
    )
    with pytest.MonkeyPatch.context() as monkeypatch:
        monkeypatch.setenv("SHELTERSIM_LLM_MODE", "offline")
        _, timeline = saved_timeline(tmp_path_factory.mktemp("closure") / "projects", scenario)
        timeline.advance("main", START, checkpoint_interval=START)
        world = timeline.seek("main", START).world
        minute = min(
            event.due_minute
            for event in world.event_queue.pending()
            if event.event_type == "support_request_timeout" and event.due_minute > START + 1
        )
        spec = DEFAULT_PRIMITIVE_REGISTRY.get("set_route_access")
        program = EffectProgram(
            program_id="program-closure",
            narrative="closure",
            source_text="closure",
            simulation_start_minute=START,
            effects=[
                TimedAction(
                    effect_id="closure",
                    op="set_route_access",
                    target_selector=TargetSelector(kind="route", target_id="route_corridor_toilet"),
                    start_minute=START,
                    end_minute=minute,
                    parameters={"accessible": False},
                    units="boolean",
                    source_id="TEST-CLOSURE",
                    assumption_ids=[],
                    policy_dependencies=sorted(spec.required_policies),
                    reversible=True,
                )
            ],
            assumptions=[],
            support_mode=SupportMode.NATIVE,
            compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
        )
        timeline.fork(
            "main",
            START,
            "closure",
            program=program,
            approved_program_hash=digest(program.model_dump(mode="json")),
        )
        timeline.advance("closure", minute, checkpoint_interval=minute - START)
        yield timeline, "closure", minute


def _applied_at(world, minute, event_type):
    return [
        record
        for record in world.event_log.at_minute(minute)
        if record.event.event_type == event_type and record.status is EventStatus.APPLIED
    ]


def test_an_expiry_in_the_minute_of_a_support_timeout_runs_on(closure_at_a_timeout):
    """#88 (元 #309): the expiry's copy of the world used to fail its validation."""
    timeline, branch, minute = closure_at_a_timeout

    world = timeline.advance(branch, minute + 1, checkpoint_interval=1).world

    timeouts = _applied_at(world, minute, "support_request_timeout")
    assert timeouts
    assert _applied_at(world, minute, "effect_program_geometry_expire")
    for record in timeouts:
        assert care_receipt_for_request(world, record.event.payload["request_id"]) is None


def test_a_timed_out_care_request_is_valid_only_until_the_end_of_its_dispatch(
    closure_at_a_timeout,
):
    """#88 (元 #309): between the batch and its care records, the world of that minute is valid.

    The rule names that minute: a world whose minute has moved on with the record still missing
    is broken, which validate_after_tick checks.
    """
    timeline, branch, minute = closure_at_a_timeout
    world = timeline.seek(branch, minute).world
    controller = OfflineSimulationController(world, _NoArtifacts())

    # The dispatch phase's batch, before the care records of what it applied.
    outcomes = controller.event_dispatcher.dispatch_due(
        world,
        minute,
        after_event=lambda records: ingest_applied_shared_operation_records(world, records),
    )

    timed_out = [
        record.event.payload["request_id"]
        for record in outcomes
        if record.event.event_type == "support_request_timeout"
        and record.status is EventStatus.APPLIED
        and care_receipt_for_request(world, record.event.payload["request_id"]) is not None
    ]
    assert timed_out
    world.validate()
    world.minute_index += 1
    with pytest.raises(StateInvariantError) as stopped:
        world.validate()
    assert str(stopped.value.__cause__) == (
        "shared_operations_v1 care status disagrees with the persisted support request"
    )

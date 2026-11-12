"""The rest of events/handlers.py's kernels: the runs that reach them, and their Python edges.

benchmarks/rule_shadow/shadow_handlers_rest.py does this on the gate's runs; 7 days hold almost none
of these events, so here each rule also runs on Scenarios made to reach it
(docs/rust-port/lanes/m6-handlers-rest.md). None of these handlers has its Python rule left (lane
L10, `kernel_only`): a run is checked against what the Python rules' run gave, written down from it
before they went, by a digest of the world (`_digest`).

The cases of one event, a care, a receipt or a draw are the core's tests now
(native/core/tests/handler_rules.rs, #225), on the worlds this file built; here stay the runs, the
entries that only these tests reach, and the values only Python holds (a host's upgrade, a patched
handler, a route resolver or a policy given from Python).

Model version 2 (step 4, L11: the pinned libm crate and the information events' draw keys)
changed some of these results: those digests were re-recorded from the kernels of that model
and are no longer the deleted Python rules'.
"""

from __future__ import annotations

import hashlib
import json
import sys
from collections import Counter
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from support.paths import SMALL_REPLAY

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.domain.information import upgrade_information_runtime_state
from sheltersim.domain.models import (
    Activity,
    InformationSpreadConfig,
    SupportRequestState,
)
from sheltersim.domain.simulator_state import initialize_simulator_extensions
from sheltersim.domain.staff import StaffService
from sheltersim.events.handlers import (
    FacilityOutageHandler,
    InformationContactHandler,
    ResourceLedgerHandler,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventStatus,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink

# The handlers whose outcomes a run counts, with their kernels.
KERNELS = {
    "ProximityContactHandler": ("handlers", "proximity"),
    "InformationRumorHandler": ("information_events", "rumor"),
    "InformationCorrectionHandler": ("information_events", "correction"),
    "InformationContactHandler": ("information_events", "contact"),
    "ResourceLedgerHandler": ("handlers", "resource_ledger"),
    "FacilityOutageHandler": ("handlers", "facility_outage"),
    "FacilityRecoveryHandler": ("handlers", "facility_recovery"),
    "InfrastructureEventHandler": ("handlers", "infrastructure"),
    "DeliveryDelayHandler": ("handlers", "delivery_delay"),
    "WaterOutageHandler": ("handlers", "water_outage"),
    "WaterRestorationHandler": ("handlers", "water_restoration"),
    "StaffUnavailabilityHandler": ("handlers", "staff_unavailable"),
    "StaffRestorationHandler": ("handlers", "staff_restoration"),
    "StaffOperationArrivalHandler": ("staff_operation_events", "arrive"),
    "StaffOperationReliefHandler": ("staff_operation_events", "relief"),
    "StaffOperationCompletionHandler": ("staff_operation_events", "complete"),
    "SocialInteractionHandler": ("social_interaction", "apply"),
    "AgentTravelCompletionHandler": ("travel_completion", "apply"),
}


def _gate() -> Any:
    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    return gate_scenarios


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _outcome(call: Any) -> tuple[str, Any]:
    try:
        result = call()
    except EventSuppressed as raised:
        return "suppressed", str(raised)
    except EventApplicationError as raised:
        return "failed", str(raised)
    except native.Unsupported as raised:
        return "refused", str(raised)
    return "applied", [record.to_dict() for record in result or ()]


def _digest(value: Any) -> str:
    """A digest of a JSON value, to pin what the Python rule gave."""
    text = json.dumps(value, sort_keys=True, ensure_ascii=False)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


def _copies(world: WorldState) -> tuple[WorldState, WorldState]:
    document = world.to_dict()
    return WorldState.from_dict(document), WorldState.from_dict(document)


class _Comparing:
    """A handler that counts the outcomes of the real one, which it runs.

    No handler here has its Python rule left (lane L10, `kernel_only`): a run is compared with the
    Python rules' run by the digest of its world (`_is_the_python_rules_run`).
    """

    def __init__(self, handler: Any, seen: Counter[str]) -> None:
        self.handler = handler
        self.seen = seen

    def apply(self, world: WorldState, event: ScheduledEvent) -> Any:
        outcome = "applied"
        try:
            return self.handler.apply(world, event)
        except EventSuppressed:
            outcome = "suppressed"
            raise
        except EventApplicationError:
            outcome = "failed"
            raise
        finally:
            self.seen[f"{event.event_type}.{outcome}"] += 1


def _run_comparing(scenario: Any, seen: Counter[str]) -> WorldState:
    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    handlers = controller.event_dispatcher.handlers
    for event_type, handler in list(handlers.items()):
        if type(handler).__name__ in KERNELS:
            handlers[event_type] = _Comparing(handler, seen)
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(world.duration_minutes):
        engine.step()
    return world


def _is_the_python_rules_run(world: WorldState, digest: str) -> None:
    """The world at the end is the one the Python rules made: its digest, taken from that run.

    Pinned on every OS, as the gate's hashes are.
    """
    assert _digest(_bytes(world)) == digest


def test_conditions_of_the_form_are_python_s() -> None:
    gate = _gate()
    conditions = {
        "scheduled": [
            {
                "kind": "power_outage",
                "component_id": "grid-main",
                "start_minute": 20,
                "end_minute": 60,
            },
            {
                "kind": "equipment_failure",
                "facility_id": "water_station_main",
                "units_out": 1,
                "start_minute": 30,
                "end_minute": 70,
            },
            {"kind": "extreme_heat", "zone_id": "sleeping", "start_minute": 10, "end_minute": 50},
            {"kind": "extreme_cold", "zone_id": "food", "start_minute": 55, "end_minute": 80},
            {
                "kind": "staff_unavailable",
                "staff_id": "staff_001",
                "start_minute": 15,
                "end_minute": 45,
                "repeat_every_minutes": 40,
            },
            {
                "kind": "resource_loss",
                "resource_id": "water_ml",
                "quantity": 2_000,
                "due_minute": 50,
            },
            {
                "kind": "resource_delivery",
                "resource_id": "food_meal_units",
                "quantity": 40,
                "due_minute": 60,
            },
        ],
        "initial_resources": {
            "status": "entered",
            "food_meal_units": 20,
            "water_ml": 8_000,
            "source_kind": "operator",
        },
        "infrastructure": {
            "dependency_edges": [
                {"dependent_id": "toilet_01", "dependency_id": "panel-main"},
                {"dependent_id": "accessible_toilet_01", "dependency_id": "panel-main"},
                {"dependent_id": "panel-main", "dependency_id": "grid-main"},
            ],
            "heat_stress_per_minute": 0.001,
            "cold_stress_per_minute": 0.001,
            "source_id": gate.SOURCE_ID,
        },
    }
    seen: Counter[str] = Counter()
    world = _run_comparing(
        gate.generated(evacuees=20, minutes=120, operational_conditions=conditions), seen
    )
    _is_the_python_rules_run(world, "f5758af05264f6df")
    for event_type in (
        "power_outage",
        "power_restored",
        "equipment_failure",
        "equipment_repaired",
        "extreme_heat",
        "extreme_cold",
        "environmental_stressor_end",
        "staff_unavailable",
        "staff_restored",
        "resource_loss",
        "resource_delivery",
    ):
        assert seen[f"{event_type}.applied"] > 0, (event_type, seen)


def test_rumors_and_corrections_are_python_s() -> None:
    gate = _gate()
    seen: Counter[str] = Counter()
    world = _run_comparing(gate.with_rumors(gate.generated(evacuees=20, minutes=120)), seen)
    _is_the_python_rules_run(world, "44de998728962e5d")
    for event_type in ("information_rumor", "information_correction", "information_contact_update"):
        assert seen[f"{event_type}.applied"] > 0, (event_type, seen)


def _schema_1(state: dict[str, Any]) -> None:
    """The information state as a save of schema 1 holds it: beliefs without the summary fields."""
    state["schema_version"] = 1
    for beliefs in state["beliefs"].values():
        for record in beliefs.values():
            for name in ("ever_misbelieved", "ever_forgotten", "last_correction_accepted"):
                del record[name]


def test_an_information_event_upgrades_a_state_of_schema_1_first() -> None:
    """A state of schema 1 in memory is upgraded in place before the event's checks.

    A save of schema 1 is view-only (core/model_version.py `LEGACY_SHAPES_TICKABLE` is false), so
    no restored Run reaches this. The handlers' Python rule upgraded such a state in place; the
    kernels do the same: the tick is the one of a state upgraded before it.
    """
    gate = _gate()
    scenario = gate.with_rumors(gate.generated(evacuees=20, minutes=120))
    worlds = []
    for upgraded_before in (False, True):
        world = WorldState.from_scenario(scenario)
        controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
        engine = TickEngine(world, phase_handler=controller)
        for _ in range(60):
            engine.step()
        state = world.model_state["information_spread_v1"]
        _schema_1(state)
        if upgraded_before:
            upgrade_information_runtime_state(state)
        engine.step()
        corrected = world.event_log.find("correction-false")
        assert corrected is not None and corrected.status is EventStatus.APPLIED
        assert world.model_state["information_spread_v1"]["schema_version"] == 2
        worlds.append(world)
    assert _bytes(worlds[0]) == _bytes(worlds[1])


def test_an_information_state_the_upgrade_turns_down_fails_the_event_before_its_checks() -> None:
    """The host's upgrade (policy E11) runs where the Python rule ran it: after the model's checks
    and before the event's, so a payload that does not fit fails with the upgrade's text, and a
    state of an unknown schema is changed by nothing."""
    gate = _gate()
    world = WorldState.from_scenario(gate.with_rumors(gate.generated(evacuees=20, minutes=120)))
    world.model_state["information_spread_v1"]["schema_version"] = 99
    before = _bytes(world)
    event = ScheduledEvent(
        event_id="rumor-test",
        kind=EventKind.EXOGENOUS,
        due_minute=world.minute_index,
        priority=0,
        sequence=900_001,
        event_type="information_contact_update",
        payload={},
        source_id="SRC-TEST",
    )
    said = _outcome(lambda: InformationContactHandler().apply(world, event))
    assert said == (
        "failed",
        "invalid information-spread state: unsupported information-spread runtime state schema",
    )
    assert _bytes(world) == before


@pytest.mark.parametrize(
    ("name", "digest"),
    [
        ("multi_operational", "666880beb4cb8d08"),
        ("water_intervention", "26726ee9ede8b09d"),
        ("failure_day3", "7228ef1bb1598fdc"),
        ("delivery_intervention", "00f630574725b910"),
    ],
)
def test_the_operational_fixtures_are_python_s(name: str, digest: str) -> None:
    gate = _gate()
    moved = {
        "resource_delivery": 20,
        "delivery_delayed": 10,
        "water_outage": 30,
        "water_restored": 50,
        "facility_outage": 20,
        "facility_restored": 60,
    }
    seen: Counter[str] = Counter()
    world = _run_comparing(gate.fixture_short(name, minutes=100, agents=20, remap=moved), seen)
    _is_the_python_rules_run(world, digest)
    assert sum(seen.values()) > 0


def _small_world(generated_scenario: Any) -> tuple[WorldState, OfflineSimulationController]:
    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(5):
        engine.step()
    return world, controller


def _event(
    world: WorldState, event_type: str, payload: dict[str, Any], **fields: Any
) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=fields.pop("event_id", f"test-{event_type.replace('_', '-')}"),
        kind=fields.pop("kind", EventKind.EXOGENOUS),
        due_minute=world.minute_index,
        priority=0,
        sequence=fields.pop("sequence", 900_001),
        event_type=event_type,
        payload=payload,
        source_id=fields.pop("source_id", "SRC-TEST"),
        causation_id=fields.pop("causation_id", None),
        provenance=fields.pop("provenance", {"source_type": "assumption", "source_id": "SRC-TEST"}),
    )


# ---- What a kernel leaves to the Python rule ----------------------------------------------------


def test_a_patched_handler_is_what_the_loop_calls(
    generated_scenario: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world, controller = _small_world(generated_scenario)
    called: list[str] = []

    def fake(_self: Any, _world: Any, event: ScheduledEvent) -> None:
        called.append(event.event_id)

    monkeypatch.setattr(ResourceLedgerHandler, "apply", fake)
    event = _event(
        world,
        "resource_delivery",
        {"warehouse_id": "warehouse_main", "resource_id": "water_ml", "quantity": 1},
        event_id="delivery-now",
    )
    world.schedule_event(event)
    TickEngine(world, phase_handler=controller).step()
    assert called == ["delivery-now"]
    assert world.event_log.find("delivery-now").status is EventStatus.APPLIED


# ---- The assignment of a request -----------------------------------------------------------------


# The world after `StaffService.assign_pending`'s Python rule, by test: a digest taken before the
# rule went (lane L10, `kernel_only`).
ASSIGNED_DIGESTS = {
    "free_worker": "b2e57bbfee3b75ed",
}


def _assign(label: str, world: WorldState, **options: Any) -> Any:
    """`assign_pending` of a service without a route, and the world it leaves against the Python
    rule's."""
    actual = StaffService(world, **options).assign_pending(world.minute_index, validate_state=False)
    _is_the_python_rules_run(world, ASSIGNED_DIGESTS[label])
    return actual


def _staffed_world() -> WorldState:
    """A world whose staff are all free, with no shifts to read."""
    document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    document["schema_version"] = 3
    document["duration_minutes"] = 200
    document["scheduled_events"] = []
    document["model_configs"]["staff_operations"] = {}
    document["model_configs"]["shared_operations"] = {
        "schema_version": 1,
        "policy_version": "shared-operations-v1",
    }
    template = document["staff"][0]
    document["staff"] = [
        {**template, "id": f"S{index:02}", "skills": ["support"]} for index in range(1, 4)
    ]
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    initialize_simulator_extensions(world)
    world.minute_index = 10
    return world


def _open_request(world: WorldState, *, age: int = 0) -> str:
    agent_id = sorted(world.agents)[0]
    agent = world.agents[agent_id]
    for key in list(world.active_tasks):
        if world.active_tasks[key].agent_id == agent_id:
            del world.active_tasks[key]
    agent.activity = Activity.SEEKING_HELP
    agent.target_id = None
    agent.support_request_id = "support-t1"
    world.support_requests["support-t1"] = SupportRequestState(
        "support-t1", agent_id, world.minute_index - age, world.minute_index + 50
    )
    return "support-t1"


def test_another_route_resolver_than_the_controller_s_is_refused() -> None:
    """The service's route and trip are the controller's own steps, which the kernel calls
    directly (lane P8 b3); another callable is refused before anything is written. The decisions
    of a route are tested with the controller's own (test_staff_assignment.py)."""
    world = _staffed_world()
    _open_request(world)
    before = _bytes(world)
    service = StaffService(world, route_resolver=lambda *_: None, travel_scheduler=lambda *_: True)
    with pytest.raises(KernelRefusal, match="another route resolver"):
        service.assign_pending(world.minute_index, validate_state=False)
    assert _bytes(world) == before


def test_a_travel_scheduler_without_a_route_resolver_is_never_asked() -> None:
    """Without a route the request is served where it stands, as the Python rule served it."""
    world = _staffed_world()
    request_id = _open_request(world)
    assert _assign("free_worker", world, travel_scheduler=lambda *_: False) == (request_id,)


# ---- What else the handlers ask ------------------------------------------------------------------


def test_only_the_due_requests_go_to_the_policy(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    request_id = _open_request(world, age=7)
    asked: list[tuple[str, str]] = []
    policy = SimpleNamespace(
        limits=SimpleNamespace(complaint_wait_minutes=7),
        consider_complaint_response=lambda _world, *, agent_id, request_id: asked.append(
            (agent_id, request_id)
        ),
    )
    controller = SimpleNamespace(world=world, social_policy=policy)
    native.rules("staff").consider_due_complaints(controller, world.minute_index)
    # The Python step's answer: the one request due this minute.
    assert asked == [(world.support_requests[request_id].agent_id, request_id)]
    asked.clear()
    native.rules("staff").consider_due_complaints(controller, world.minute_index + 1)
    assert asked == []
    controller.social_policy = None
    native.rules("staff").consider_due_complaints(controller, world.minute_index)


def test_ingest_of_a_record_of_this_tick_s_entry_records_it_once(generated_scenario: Any) -> None:
    from support.receipts import facility_recovery_record

    from sheltersim.domain import care_receipts

    world, _ = _small_world(generated_scenario)
    facility_id = sorted(world.facilities)[0]
    record = facility_recovery_record(
        world,
        facility_id=facility_id,
        component_id="component-1",
        status="degraded",
        previous_working_units=2,
        working_units=1,
        causation_id="cause-1",
    )[0]
    world.event_log.append(record)
    care_receipts.ingest_applied_shared_operation_records(world, (record,))
    history = world.model_state["shared_operations_v1"]["history"]
    assert history[-1]["event_id"] == record.event.event_id
    once = _bytes(world)
    care_receipts.ingest_applied_shared_operation_records(world, (record,))
    assert _bytes(world) == once


def test_the_kernels_are_what_ran(generated_scenario: Any) -> None:
    world, _ = _small_world(generated_scenario)
    before, _ = native.rules("handlers").stats()
    handler = FacilityOutageHandler()
    facility_id = next(key for key, item in sorted(world.facilities.items()) if item.working_units)
    event = _event(world, "facility_outage", {"facility_id": facility_id, "units_out": 1})
    handler.apply(world, event)
    after, _ = native.rules("handlers").stats()
    assert after["facility_outage"] > before.get("facility_outage", 0)


# ---- What a draw hides ---------------------------------------------------------------------------


def test_the_draw_entries_of_a_claim_and_a_correction_are_python_s(generated_scenario: Any) -> None:
    """The entries of a claim's and a correction's draws give Python's CounterRNG's answers. The
    core's test draws 4000 of each, and a seed past 64 bits."""
    world, _ = _small_world(generated_scenario)
    agent_id = sorted(world.agents)[0]
    world.agents[agent_id].trust = 1.0
    rules = native.rules("information_events")
    # The belief's Python rule: 1.0 when the draw is below the clamped probability, else 0.0.
    believing_probability = min(1.0, max(0.0, 0.0 + 2.0 * 0.5 * (1.0 - 0.5) + 0.0))
    believing = InformationSpreadConfig(
        rumor_belief_probability=0.0, rumor_trust_sensitivity=0.5, rumor_source_sensitivity=0.0
    )
    accepting = InformationSpreadConfig(
        correction_acceptance_probability=1.0,
        correction_trust_sensitivity=0.0,
        correction_source_sensitivity=0.0,
    )
    rng = CounterRNG(world.seed)
    # Model version 2 keys the draws by the minute and the people, the source and the message,
    # without the event's ID.
    for number in range(100):
        message_id = f"claim-{number}"
        belief_draw = rng.random("rumor-belief-acceptance-v1", 7, agent_id, message_id)
        python_strength = 1.0 if belief_draw < believing_probability else 0.0
        assert python_strength == rules.claim_strength_of(
            world, believing, agent_id, "SRC-TEST", 7, message_id
        )
        label = "correction-acceptance-v1"
        drawn = rng.random(label, 7, "SRC-TEST", agent_id, message_id)
        assert (drawn < 0.95) == rules.acceptance_of(
            world, accepting, label, 1.0, "SRC-TEST", 7, ["SRC-TEST", agent_id, message_id]
        )

"""Each handler's PyO3 entry reaches its kernel, which raises its own outcome with the saved text.

The texts are saved: the dispatcher writes them to the event's record as `detail`. The core's
tests check every text of every handler, with the class the Python rule raised, on worlds the
Python side built (`native/core/tests/event_texts.rs`, `social_interaction_texts.rs` and
`travel_completion_texts.rs`, from `tests/support/core_texts.py`; #225). This file keeps one case of
each handler, through its `kernel_only` entry: the kernel raises the outcome itself (not a refusal),
changes nothing, and its class and text are the Python rule's (support/world.py
`kernel_says_python_s_text`). See docs/rust-port/lanes/l8-texts-events.md.
"""

from __future__ import annotations

import copy
import sys
from pathlib import Path
from typing import Any

import pytest
from support.world import kernel_says_python_s_text

from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
from sheltersim.events.effects import EffectProgramFacilityHandler
from sheltersim.events.handlers import (
    DeliveryDelayHandler,
    EffectProgramPopulationHandler,
    FacilityOutageHandler,
    FacilityRecoveryHandler,
    InformationContactHandler,
    InformationCorrectionHandler,
    InformationRumorHandler,
    InfrastructureEventHandler,
    ProximityContactHandler,
    ResourceLedgerHandler,
    SocialInteractionHandler,
    StaffOperationArrivalHandler,
    StaffOperationCompletionHandler,
    StaffOperationReliefHandler,
    StaffRestorationHandler,
    StaffSupportCompletionHandler,
    StaffUnavailabilityHandler,
    WaterOutageHandler,
    WaterRestorationHandler,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink


@pytest.fixture(scope="module")
def controlled_world() -> tuple[WorldState, OfflineSimulationController]:
    """The gate's generated Scenario at minute 5, and its controller."""
    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    world = WorldState.from_scenario(gate_scenarios.generated(evacuees=20, minutes=120))
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(5):
        engine.step()
    return world, controller


def _event(event_type: str, payload: dict[str, Any], **fields: Any) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=f"test-{event_type.replace('_', '-')}",
        kind=fields.pop("kind", EventKind.EXOGENOUS),
        due_minute=5,
        priority=0,
        sequence=900_001,
        event_type=event_type,
        payload=payload,
        source_id=fields.pop("source_id", "SRC-TEST"),
        causation_id=None,
        provenance=fields.pop("provenance", {"source_type": "assumption", "source_id": "SRC-TEST"}),
    )


_FAILED = EventApplicationError
_OPERATION = {"staff_id": "staff_001", "task_id": "nothing", "target_zone_id": "sleeping"}
_SOCIAL = {
    "source_id": "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1",
    "source_type": "assumption",
    "policy_version": "social-v1",
}

# One case of each handler's entry: the handler (a str is the controller's handler of that event
# type), the event, and the Python rule's outcome.
CASES: dict[str, tuple[Any, ScheduledEvent, tuple[type, str]]] = {
    "facility_outage": (
        FacilityOutageHandler(),
        _event("facility_outage", {"facility_id": "nowhere", "units_out": 1}),
        (_FAILED, "facility 'nowhere' does not exist"),
    ),
    "facility_recovery": (
        FacilityRecoveryHandler(),
        _event("facility_restored", {"facility_id": "toilet_01", "units_restored": 0}),
        (EventSuppressed, "zero-unit facility recovery has no effect"),
    ),
    "proximity": (
        ProximityContactHandler(),
        _event("proximity_contact_update", {}),
        (_FAILED, "proximity contact provenance or time is invalid"),
    ),
    "resource_ledger": (
        ResourceLedgerHandler("delivery"),
        _event(
            "resource_delivery",
            {"warehouse_id": "main", "resource_id": "water_ml", "quantity": 1, "shipment_id": "x"},
        ),
        (_FAILED, "shipment delivery does not match operational state"),
    ),
    "delivery_delay": (
        DeliveryDelayHandler(),
        _event("delivery_delayed", {"shipment_id": "x"}),
        (_FAILED, "delivery delay payload fields or source are invalid"),
    ),
    "water_outage": (
        WaterOutageHandler(),
        _event("water_outage", {"outage_id": "w", "zone_id": "z", "x": 1}),
        (_FAILED, "water outage payload fields or source are invalid"),
    ),
    "water_restoration": (
        WaterRestorationHandler(),
        _event("water_restored", {"outage_id": "w"}, source_id=None),
        (_FAILED, "water restoration payload fields or source are invalid"),
    ),
    "staff_unavailable": (
        StaffUnavailabilityHandler(),
        _event("staff_unavailable", {"staff_id": "staff_001"}),
        (_FAILED, "staff unavailability payload fields or source are invalid"),
    ),
    "staff_restoration": (
        StaffRestorationHandler(),
        _event("staff_restored", {"staff_id": "staff_001"}),
        (_FAILED, "staff restoration payload fields or source are invalid"),
    ),
    "infrastructure": (
        InfrastructureEventHandler(),
        _event("power_outage", {"outage_id": "o"}),
        (_FAILED, "power_outage requires the opted-in infrastructure model"),
    ),
    "population": (
        EffectProgramPopulationHandler(),
        _event("population_primitive", {"op": "teleport"}),
        (_FAILED, "population primitive operation is invalid"),
    ),
    "information_rumor": (
        InformationRumorHandler(),
        _event("information_rumor", {"x": 1}),
        (_FAILED, "information_rumor payload fields do not match"),
    ),
    "information_correction": (
        InformationCorrectionHandler(),
        _event("information_correction", {"x": 1}),
        (_FAILED, "information_correction payload fields do not match"),
    ),
    "information_contact": (
        InformationContactHandler(),
        _event("information_contact_update", {"x": 1}),
        (_FAILED, "information_contact_update payload fields do not match"),
    ),
    "staff_operation_arrival": (
        StaffOperationArrivalHandler(),
        _event("staff_operation_complete", _OPERATION),
        (_FAILED, "staff operation arrival handler received another event"),
    ),
    "staff_operation_completion": (
        StaffOperationCompletionHandler(),
        _event(
            "staff_operation_arrive", {**_OPERATION, "task_kind": "cleaning", "target_id": None}
        ),
        (_FAILED, "staff operation completion handler received another event"),
    ),
    "staff_operation_relief": (
        StaffOperationReliefHandler(),
        _event("staff_operation_relief", {"task_id": "nothing"}),
        (_FAILED, "staff operation relief provenance or payload is invalid"),
    ),
    "staff_support_completion": (
        StaffSupportCompletionHandler(),
        _event(
            "staff_support_complete",
            {"request_id": "r", "agent_id": "A001", "staff_id": "staff_001", "task_id": ""},
        ),
        (_FAILED, "staff completion requires request, Agent, staff, and task IDs"),
    ),
    "effect_program_facility": (
        EffectProgramFacilityHandler(),
        _event("facility_availability", {"program_id": "p", "effect_id": "e"}),
        (_FAILED, "effect program apply payload fields do not match"),
    ),
    "travel_completion": (
        "agent_travel_complete",
        _event(
            "agent_travel_complete",
            {
                "agent_id": "A001",
                "task_id": "nothing",
                "destination_zone_id": "sleeping",
                "purpose": "return",
            },
        ),
        (_FAILED, "travel completion does not match an active task"),
    ),
    "toilet_completion": (
        "toilet_service_complete",
        _event(
            "toilet_service_complete",
            {"facility_id": "toilet_01", "agent_id": "A001", "task_id": "nothing"},
        ),
        (_FAILED, "toilet completion does not match an active due task"),
    ),
    "support_timeout": (
        "support_request_timeout",
        _event("support_request_timeout", {"request_id": "nothing", "agent_id": "A001"}),
        (_FAILED, "support request 'nothing' does not match its Agent"),
    ),
    "social_interaction": (
        SocialInteractionHandler(),
        _event(
            "social_interaction",
            {"interaction_type": "cooperation", "agent_id": "nobody", "reason_short": "x"},
            kind=EventKind.ENDOGENOUS,
            source_id="social-v1",
            provenance=_SOCIAL,
        ),
        (_FAILED, "typed social interaction references an unknown Agent"),
    ),
}


@pytest.mark.parametrize("name", sorted(CASES))
def test_a_handler_s_entry_raises_its_kernel_s_text(
    controlled_world: tuple[WorldState, OfflineSimulationController], name: str
) -> None:
    world, controller = controlled_world
    handler, event, expected = CASES[name]
    if isinstance(handler, str):
        handler = controller.event_dispatcher.handlers[handler]
    world = copy.deepcopy(world)
    said = kernel_says_python_s_text(handler, world, event, expected)
    assert said == (expected[0].__name__, expected[1])

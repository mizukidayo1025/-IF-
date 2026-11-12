"""Receipts of the shared operation contract and their applied records, made in Python.

What `make_care_transition_record` and `make_facility_recovery_record` of
domain/care_receipts.py made before the kernels made them (native/core/src/rules/care_receipts.rs,
#152 P8b-1): the input of a test of `record_shared_operation` or of the ingest of receipts. Each
allocates the world's next event sequence, as the kernel does.
"""

from __future__ import annotations

from sheltersim.contracts.operations import (
    SHARED_OPERATIONS_KEY,
    CareTransitionReceipt,
    FacilityRecoveryReceipt,
    NumericStateChange,
)
from sheltersim.contracts.population import population_context_people
from sheltersim.core.world import WorldState
from sheltersim.events.models import EventKind, EventLogRecord, EventStatus, ScheduledEvent

CARE_SOURCE = "ASM-CARE-TRANSITION-PRODUCER-V1"
CARE_POLICY = "applied-care-transition-v1"
FACILITY_SOURCE = "ASM-FACILITY-RECOVERY-PRODUCER-V1"


def care_transition_record(
    world: WorldState,
    *,
    request_id: str,
    agent_id: str,
    household_id: str | None,
    support_kind: str,
    status: str,
    staff_id: str | None = None,
    outcome_observation: str = "not_applicable",
    state_changes: tuple[NumericStateChange, ...] = (),
    causation_id: str | None = None,
    source_ids: tuple[str, ...] = (),
) -> tuple[EventLogRecord, CareTransitionReceipt]:
    """Create a bounded applied evidence event for one observed care transition."""
    sequence = world.allocate_event_sequence()
    event_id = f"care-support-transition-{sequence}"
    all_sources = tuple(sorted({CARE_SOURCE, *source_ids}))
    receipt = CareTransitionReceipt(
        event_id=event_id,
        request_id=request_id,
        agent_id=agent_id,
        household_id=household_id,
        support_kind=support_kind,
        status=status,
        minute=world.minute_index,
        staff_id=staff_id,
        outcome_observation=outcome_observation,
        state_changes=state_changes,
        causation_id=causation_id,
        source_ids=all_sources,
    )
    event = ScheduledEvent(
        event_id=event_id,
        kind=EventKind.ENDOGENOUS,
        due_minute=world.minute_index,
        priority=0,
        sequence=sequence,
        event_type=receipt.event_type,
        payload=receipt.model_dump(mode="json"),
        source_id=CARE_SOURCE,
        causation_id=causation_id,
        provenance={
            "source_type": "assumption",
            "source_id": CARE_SOURCE,
            "policy_version": CARE_POLICY,
        },
    )
    return (
        EventLogRecord(
            event=event,
            realized_minute=world.minute_index,
            status=EventStatus.APPLIED,
            detail=f"care transition {status} recorded after state application",
        ),
        receipt,
    )


def facility_recovery_record(
    world: WorldState,
    *,
    facility_id: str,
    component_id: str,
    status: str,
    previous_working_units: int,
    working_units: int,
    causation_id: str,
    source_ids: tuple[str, ...] = (),
) -> tuple[EventLogRecord, FacilityRecoveryReceipt] | None:
    """Create a recovery receipt only when the shared operation contract is enabled."""
    if SHARED_OPERATIONS_KEY not in world.model_state:
        return None
    sequence = world.allocate_event_sequence()
    event_id = f"facility-recovery-transition-{sequence}"
    all_sources = tuple(sorted({FACILITY_SOURCE, *source_ids}))
    receipt = FacilityRecoveryReceipt(
        event_id=event_id,
        facility_id=facility_id,
        component_id=component_id,
        status=status,
        minute=world.minute_index,
        previous_working_units=previous_working_units,
        working_units=working_units,
        source_ids=all_sources,
        causation_id=causation_id,
    )
    event = ScheduledEvent(
        event_id=event_id,
        kind=EventKind.ENDOGENOUS,
        due_minute=world.minute_index,
        priority=0,
        sequence=sequence,
        event_type=receipt.event_type,
        payload=receipt.model_dump(mode="json"),
        source_id=FACILITY_SOURCE,
        causation_id=causation_id,
        provenance={
            "source_type": "assumption",
            "source_id": FACILITY_SOURCE,
            "policy_version": "applied-facility-recovery-v1",
        },
    )
    return (
        EventLogRecord(
            event=event,
            realized_minute=world.minute_index,
            status=EventStatus.APPLIED,
            detail=f"facility {status} receipt recorded after working-unit update",
        ),
        receipt,
    )


def care_source_ids(world: WorldState, agent_id: str) -> tuple[str, ...]:
    """The sources of a care receipt of `agent_id`: the staff support lifecycle's and the person's
    own str sources of `population_context_v1`, sorted. What `care_source_ids_for_agent` of
    domain/care_receipts.py gave before the kernels read them themselves (deleted in lane P9-3b of
    #152: no rule called it).
    """
    person = population_context_people(world).get(agent_id, {})
    sources = person.get("source_ids", ()) if isinstance(person, dict) else ()
    return tuple(
        sorted(
            {
                "ASM-STAFF-SUPPORT-LIFECYCLE-V1",
                *(item for item in sources if isinstance(item, str)),
            }
        )
    )

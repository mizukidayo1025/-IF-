"""Event-backed producer helpers for the shared care operation contract.

This module projects only care transitions that already happened in the WorldState.
It does not create a support request or claim a completed outcome by itself. The receipts and
their events are made by native/core/src/rules/care_receipts.rs.
"""

from __future__ import annotations

from typing import Any

from sheltersim.contracts.operations import (
    SHARED_OPERATIONS_KEY,
    CareTransitionReceipt,
    StaffSupportContinuityReceipt,
)
from sheltersim.contracts.population import population_context_people
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import read_only_part
from sheltersim.core.world import WorldState
from sheltersim.events.models import EventStatus


@kernel_only("care_receipts", "append_transition")
def append_applied_care_transition(
    world: WorldState,
    *,
    request_id: str,
    agent_id: str,
    household_id: str | None,
    support_kind: str,
    status: str,
    staff_id: str | None = None,
    causation_id: str | None = None,
    source_ids: tuple[str, ...] = (),
) -> None:
    """Append and project a care transition after its backing operation has applied.

    Nothing without shared_operations_v1. Else the applied record of a new
    care_support_transition event (care-support-transition-{sequence}, source
    ASM-CARE-TRANSITION-PRODUCER-V1, its payload the `CareTransitionReceipt` dump) goes to the
    EventLog and its receipt to `record_shared_operation`, whose ValueError it raises.
    """


@kernel_only("care_receipts", "ingest")
def ingest_applied_shared_operation_records(world: WorldState, outcomes: tuple[Any, ...]) -> None:
    """Project typed shared receipts only after EventLog APPLIED insertion.

    Nothing without shared_operations_v1. Each APPLIED outcome of a care_support_transition,
    staff_support_continuity, facility_recovery_transition, staff_handoff or
    staff_operation_transition event whose event_id has no record yet (among the records since the
    previous tick when it was realized now, else among all) goes to `record_shared_operation` as
    the receipt its payload is (native/core/src/rules/common_operations.rs checks it against
    `OperationHistoryRecord`; a payload the contract does not take as it is is refused).
    """


def care_receipt_for_request(world: WorldState, request_id: str) -> dict[str, Any] | None:
    """Return the active shared request projection, if this request was typed as care."""
    requests = read_only_part(world, SHARED_OPERATIONS_KEY, "support_requests")
    record = requests.get(request_id) if isinstance(requests, dict) else None
    return record if isinstance(record, dict) else None


def care_history_for_agent(
    world: WorldState, agent_id: str, *, limit: int = 20
) -> list[dict[str, Any]]:
    """Project only applied typed care receipts for one person, newest last.

    This is an inspection view over EventLog, not a second mutable care ledger.
    The immutable population context remains unchanged and no clinical outcome is
    inferred from a completed staff task.
    """
    if not isinstance(agent_id, str) or not agent_id:
        raise ValueError("agent_id must be a non-empty string")
    if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= 100:
        raise ValueError("care history limit must be between 1 and 100")
    return care_history_by_agent(world, limit=limit).get(agent_id, [])


def care_history_by_agent(world: WorldState, *, limit: int = 20) -> dict[str, list[dict[str, Any]]]:
    """Build bounded per-person views with one pass over the retained EventLog."""

    if not isinstance(limit, int) or isinstance(limit, bool) or not 1 <= limit <= 100:
        raise ValueError("care history limit must be between 1 and 100")
    # A writer renders a frame at every save. The EventLog only grows, so the rows made
    # for its earlier records are kept with it and only the new records are read.
    log = world.event_log
    position, rows = log.derived.get(("care_history", limit), (0, {}))
    # The care event types' records, in append order, without reading the others.
    for record in log.of_types_from(
        {"care_support_transition", "staff_support_continuity"}, position
    ):
        event = record.event
        if record.status is not EventStatus.APPLIED:
            continue
        if event.event_type == "staff_support_continuity":
            receipt = StaffSupportContinuityReceipt.model_validate(event.payload)
            if (
                receipt.event_id != event.event_id
                or receipt.minute != record.realized_minute
                or receipt.causation_id != event.causation_id
            ):
                continue
            evidence_semantics = "applied_staff_support_continuity_after_interruption"
        else:
            receipt = CareTransitionReceipt.model_validate(event.payload)
            if (
                receipt.event_id != event.event_id
                or receipt.minute != record.realized_minute
                or receipt.causation_id != event.causation_id
            ):
                continue
            evidence_semantics = "applied_typed_care_transition"
        row = receipt.model_dump(mode="json")
        row["realized_minute"] = record.realized_minute
        row["source_id"] = event.source_id
        row["time_semantics"] = "recorded_event_minute"
        row["evidence_semantics"] = evidence_semantics
        affected_ids = {receipt.agent_id}
        if event.event_type == "care_support_transition":
            affected_ids.update(change.entity_id for change in receipt.state_changes)
        for identity in sorted(affected_ids):
            person_row = {
                **row,
                "person_role_in_record": (
                    "request_person"
                    if identity == receipt.agent_id
                    else "explicit_related_state_change"
                ),
            }
            history = rows.setdefault(identity, [])
            history.append(person_row)
            del history[:-limit]
    log.derived[("care_history", limit)] = (len(log), rows)
    return {identity: [dict(row) for row in history] for identity, history in rows.items()}


def support_kind_for_agent(world: WorldState, agent_id: str) -> str:
    """Select only care subtypes explicitly present in synthetic population context."""
    person = population_context_people(world).get(agent_id, {})
    if not isinstance(person, dict):
        return "emotional_support"
    configured = person.get("care_support_needs", ())
    completed = world.event_log.completed_support_kinds(agent_id)
    if isinstance(configured, (list, tuple)):
        for support_kind in configured:
            if isinstance(support_kind, str) and support_kind not in completed:
                return support_kind
    if person.get("medication_support") is True:
        if "medication_support" not in completed:
            return "medication_support"
    if person.get("medical_support") is True:
        if "medical_assessment" not in completed:
            return "medical_assessment"
    return "emotional_support"


def care_source_ids_for_agent(world: WorldState, agent_id: str) -> tuple[str, ...]:
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


def explicit_care_dependents(
    world: WorldState, caregiver_id: str, *, household_id: str | None = None
) -> tuple[str, ...]:
    """Return only active, reviewed, reciprocal caregiver links in one household."""
    people = population_context_people(world)
    caregiver = people.get(caregiver_id)
    if not isinstance(caregiver, dict) or caregiver.get("requires_review", False):
        return ()
    dependents = caregiver.get("care_for_ids", ())
    caregiver_household = caregiver.get("household_id")
    if (
        not isinstance(dependents, (list, tuple))
        or not dependents
        or not isinstance(caregiver_household, str)
        or (household_id is not None and household_id != caregiver_household)
        or not caregiver.get("source_ids")
    ):
        return ()
    linked: list[str] = []
    for dependent_id in dependents:
        dependent = people.get(dependent_id)
        if (
            not isinstance(dependent_id, str)
            or dependent_id not in world.agents
            or not isinstance(dependent, dict)
            or dependent.get("requires_review", False)
            or dependent.get("household_id") != caregiver_household
            or caregiver_id not in dependent.get("caregiver_ids", ())
            or not dependent.get("source_ids")
        ):
            continue
        linked.append(dependent_id)
    return tuple(sorted(set(linked)))

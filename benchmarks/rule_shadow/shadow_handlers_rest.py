"""What the kernels of events/handlers.py did, in each process: the counts of rules/handlers.rs.

The handlers of rules/handlers.rs, information_events.rs, staff_operation_events.rs,
social_interaction.rs and travel_completion.rs, `_reject_room_approach`,
`interrupt_staff_trips_on_closed_routes`, `StaffService.assign_pending`, the receipts' `ingest` and
`_consider_due_complaints` have no Python rule left to compare (lane L10), so this shadow compares
nothing: it reports how often each kernel ran and what it turned down.
"""

from __future__ import annotations

from shadow_effects import report_stats
from shadow_kit import Recorder


def prepare(output: str) -> None:
    from sheltersim import native

    report_stats(Recorder(output, "handlers_rest"), native.rules("handlers"))

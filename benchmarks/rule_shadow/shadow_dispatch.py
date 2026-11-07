"""Compare the Rust dispatch phase with the Python phase's records.

The loop goes through each rule's Python entry here (`use_python_entries`), so the other shadows see
every call and the kernels are the ones they replace. What only the loop decides is compared here:

- Each phase: the new record of each event of the due snapshot holds the bytes Python's json writes
  for that event, those records are in the queue's order, and no event of the snapshot stays
  pending. `id_counters["event"]` never goes back.

The walk's `apply` (rules/travel_completion.rs) has no Python rule left to compare (`kernel_only`,
lane L10).

Plain runs of the gate or of 7 days say whether the direct calls ran: put SHELTERSIM_RULE_SHADOW and
SHELTERSIM_DISPATCH_COUNT_ONLY=1 in the environment, and this only writes `dispatch.stats()`.
"""

from __future__ import annotations

import atexit
import json
import os
from typing import Any

from shadow_kit import Recorder


def prepare(output: str) -> None:
    from sheltersim import native

    recorder = Recorder(output, "dispatch")
    module = native.rules("dispatch")

    def report_counts() -> None:
        counts, reasons = module.stats()
        for key, count in counts.items():
            recorder.seen[f"loop.{key}"] += count
        for key, count in reasons.items():
            recorder.reasons[f"loop: {key}"] += count

    # After Recorder's own: atexit runs the last registered first.
    atexit.register(report_counts)
    if os.environ.get("SHELTERSIM_DISPATCH_COUNT_ONLY") == "1":
        return
    module.use_python_entries(True)
    unsupported = native.Unsupported
    run_phase = module.run_phase

    def phase(controller: Any, world: Any, minute: Any) -> Any:
        recorder.seen["phase.calls"] += 1
        before = len(world.event_log)
        counter = world.id_counters.get("event", 0)
        due = {event.event_id: event for event in world.event_queue.due(minute)}
        skipped = module.stats()[0]["events_skipped"]
        try:
            result = run_phase(controller, world, minute)
        except unsupported as reason:
            recorder.reasons[f"phase: {reason}"] += 1
            raise
        except Exception as raised:
            # What the phase raises is the Python phase's too: the hashes of the run say so.
            recorder.seen[f"phase.raised.{type(raised).__name__}"] += 1
            raise
        recorder.seen["phase.kernel_ran"] += 1
        recorder.seen["phase.due_events"] += len(due)
        cancelled = module.stats()[0]["events_skipped"] - skipped
        _compare_records(recorder, world, minute, before, due, cancelled)
        if world.id_counters.get("event", 0) < counter:
            recorder.differ(f"minute {minute}: the event counter went back")
        return result

    module.run_phase = phase


def _canonical(record: Any) -> bytes:
    return json.dumps(
        record.to_dict(), sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False
    ).encode()


def _compare_records(
    recorder: Recorder, world: Any, minute: int, before: int, due: dict[str, Any], cancelled: int
) -> None:
    from sheltersim.events.models import EventLogRecord, EventStatus

    new = world.event_log.records_from(before)
    stored = world.event_log.items_bytes(before, before + len(new))
    for record, raw in zip(new, stored, strict=True):
        event = due.get(record.event.event_id)
        if event is None:
            continue
        recorder.seen["phase.records_compared"] += 1
        # The event's bytes from the object it was before the phase, by Python's encoder.
        written = _canonical(EventLogRecord(event, minute, record.status, record.detail))
        if written != raw:
            recorder.differ(f"minute {minute} {event.event_id}: {written!r} != {raw!r}")
        if (record.status is EventStatus.APPLIED) != (record.detail is None):
            recorder.differ(f"minute {minute} {event.event_id}: {record.status} {record.detail}")
    order = [record.event.event_id for record in new if record.event.event_id in due]
    if order != [event_id for event_id in due if event_id in set(order)]:
        # A handler that cancels a later due event records it before its own outcome, in Python
        # too, and the loop then skips it. Any other record out of order is a difference.
        if cancelled:
            recorder.seen["phase.records_of_cancelled_events_first"] += 1
        else:
            recorder.differ(f"minute {minute}: records out of order: {order} {list(due)}")
    pending = {event.event_id for event in world.event_queue.pending()}
    for event_id in due:
        if event_id in pending and event_id not in set(order):
            recorder.differ(f"minute {minute}: {event_id} stayed pending")

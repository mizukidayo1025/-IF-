"""Compare `record_outcome` of native/src/rules/decisions.rs with `_record_outcome` at each call.

Here each call runs the Python rules first, with the sink's method putting back what it appended.
Then the kernel runs on the real world. They must give:

- the same row in `sink.observations`, compared bit by bit with the keys in their order, and the
  same row the index reads;
- with a trace, the same document for the writer as `_decision_document`, keys in order.

The receipts of the action lifecycle and the record in `agent_dynamics_v1` that the kernel writes
(`commit_action`, `remember_decision`) have no Python rule left to compare with (lane L10).
"""

from __future__ import annotations

import json
from collections import Counter
from typing import Any

from behavior_shadow import exact
from shadow_kit import Recorder, captured, text, values_in


def prepare(output: str) -> None:
    from sheltersim import native

    rules = native.rules("decisions")
    kernel = rules.record_outcome
    recorder = Recorder(output, "decisions")
    unsupported = native.Unsupported
    row_class = rules.ProposalRow
    bits: Counter[str] = Counter()

    def run(
        controller: Any,
        proposal: Any,
        *,
        target_id: Any,
        next_decision_at: Any,
        committed: Any,
        observed_state: Any,
    ) -> None:
        options = {
            "target_id": target_id,
            "next_decision_at": next_decision_at,
            "committed": committed,
            "observed_state": observed_state,
        }
        recorder.seen["calls"] += 1
        if type(proposal) is not row_class:
            recorder.seen["python_proposals"] += 1
            kernel(controller, proposal, **options)
            return
        from sheltersim.experiments.timeline import _IndexedMetricSink

        world, sink = controller.world, controller.artifacts
        recorded = proposal.with_outcome(target_id, next_decision_at)

        expected = None
        if type(sink) is _IndexedMetricSink:
            _IndexedMetricSink.record_decision(sink, recorded, observed_state=observed_state)
            expected = sink.observations.pop()

        count = len(getattr(sink, "observations", ()))
        expected_document = None
        if vars(type(sink)).get("decision_sink") == "artifacts" and sink.trace_level != "off":
            from sheltersim.inspection.traces import _decision_document

            expected_document = text(_decision_document(recorded, full=sink.trace_level == "full"))
        try:
            with captured(sink, "record_decision_document") as documents:
                kernel(controller, proposal, **options)
        except unsupported as reason:
            recorder.reasons[f"record_outcome: {reason}"] += 1
            raise
        if expected_document is not None:
            recorder.seen["trace_documents"] += len(documents)
            mine = text(json.loads(documents[0])[0][0]) if len(documents) == 1 else None
            if mine != expected_document:
                recorder.differ(
                    f"{proposal.agent_id}: {mine!r:.800} is not {expected_document:.800}"
                )
            else:
                recorder.seen["trace_bytes_compared"] += len(mine)
                recorder.seen["trace_values_compared"] += values_in(json.loads(mine))
        recorder.seen["kernel_ran"] += 1
        recorder.seen[f"action_{recorded.action.value}"] += 1
        recorder.seen["committed"] += committed
        recorder.seen["with_target"] += target_id is not None
        where = f"minute {world.minute_index}, {proposal.agent_id}"

        if expected is None:
            recorder.seen["silent_sink"] += 1
        else:
            recorder.seen["indexed_sink"] += 1
            if len(sink.observations) != count + 1 or type(sink.observations[-1]) is not bytes:
                recorder.differ(f"{where}: the sink did not get one row of bytes")
            else:
                raw = sink.observations[-1]
                row = json.loads(raw)
                recorder.seen["row_bytes_compared"] += len(raw)
                if json.dumps(exact(row, bits)) != json.dumps(exact(expected, bits)):
                    recorder.differ(f"{where}: {row!r:.600} is not {expected!r:.600}")
                if native.index_row(row)[:4] != native.index_row(expected)[:4]:
                    recorder.differ(f"{where}: the index reads the rows differently")
                recorder.seen["row_values_compared"] += values_in(expected)
                recorder.seen["rows_with_environment"] += (
                    "zone_environment" in expected["observed_state"]
                )
                recorder.seen["rows_with_household"] += expected["household_id"] is not None

        recorder.seen["values_in_rows"] = bits["behavior_values_compared"]

    rules.record_outcome = run

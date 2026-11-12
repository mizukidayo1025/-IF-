"""What rules/decisions.rs records of a decision: the rows of `_record_outcome`.

The steps `_propose_due`, `_observed_state` and `_record_outcome` have no Python body (lane L10;
docs/rust-port/lanes/j-decisions.md). What a Run records is compared with what the Python steps
recorded of the same Run, written down from them on 45a9bc9 before they went (digests, pinned on
Linux as the gate's hashes are). What the rows don't take goes to the sink's own `record_decision`
and the domain rules, as the Python step called them. The records and the world of a Run's ticks
are checked in the core (`native/core/tests/decisions_run.rs`, #225).

Model version 2 (step 4, L11: the pinned libm crate and the information events' draw keys)
changed some of these results: those digests were re-recorded from the kernels of that model
and are no longer the deleted Python rules'.
"""

from __future__ import annotations

import copy
import hashlib
import json
from dataclasses import replace
from pathlib import Path
from typing import Any

import pytest
from support.world import driven_ticks, view_of

from sheltersim import native
from sheltersim.core.engine import TickEngine, TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.domain.population_lifecycle import current_household_id
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController, run_scenario
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _IndexedMetricSink, _MetricSink
from sheltersim.policies.behavior import ActionProposal
from sheltersim.storage import history_index
from sheltersim.storage.history_index import HistoryIndex


def _rules() -> Any:
    return native.rules("decisions")


def _exact(value: Any) -> Any:
    """Equal only with the same types, the same bits, and the same order of each dict's keys."""
    if type(value) is float:
        return value.hex()
    if isinstance(value, list):
        return [_exact(item) for item in value]
    if isinstance(value, dict):
        return [[key, _exact(item)] for key, item in value.items()]
    return [type(value).__name__, value]


def _digest(value: Any) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True).encode("utf-8")).hexdigest()[:16]


def _python_s(value: Any, digest: str) -> None:
    """`value`'s digest is the one of what the Python steps made, recorded from them on 45a9bc9.
    Pinned on every OS, as the gate's hashes are."""
    assert _digest(value) == digest


def _indexed_controller(scenario: Any) -> tuple[WorldState, OfflineSimulationController, Any]:
    world = WorldState.from_scenario(scenario)
    sink = _IndexedMetricSink(MetricsRecorder(), world)
    return world, OfflineSimulationController(world, sink), sink


def _step_until_due(world: WorldState, controller: Any, minimum: int = 1) -> tuple[Any, Any]:
    """A world about to propose, with its view and rows: the tick stops before they are applied."""
    engine = TickEngine(world, phase_handler=controller)
    while True:
        engine.step()
        view = native.rules("world_view").view_for_proposals(controller, world, world.minute_index)
        rows = _rules().propose_rows(controller, view, CounterRNG(world.seed))
        if len(rows) >= minimum:
            if hasattr(controller.artifacts, "observations"):
                controller.artifacts.observations.clear()
            return view, rows


def _record(
    controller: Any, row: Any, observed: Any, *, target: Any = None, committed: bool = True
) -> None:
    _rules().record_outcome(
        controller,
        row,
        target_id=target,
        next_decision_at=row.next_decision_at,
        committed=committed,
        observed_state=observed,
    )


def _as_dict(row: Any) -> Any:
    return json.loads(row) if type(row) is bytes else row


def test_a_tick_makes_no_proposal_object(
    generated_scenario: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    """The decisions of a tick reach a sink of rows without an `ActionProposal`, which a sink of
    objects records (`with_outcome`)."""
    made: list[Any] = []
    record = ActionProposal.with_outcome

    def counted(self: Any, *arguments: Any) -> Any:
        made.append(self)
        return record(self, *arguments)

    monkeypatch.setattr(ActionProposal, "with_outcome", counted)
    world, controller, sink = _indexed_controller(generated_scenario)
    ticks = driven_ticks()
    TickEngine(world, phase_handler=controller).run(steps=20)
    assert driven_ticks() - ticks == 20
    assert any(type(row) is bytes for row in sink.observations)
    assert all(type(row) is bytes for row in sink.observations if type(row) is not dict)
    assert not made
    # A sink whose own class declares no `decision_sink` gets the proposals.
    world = WorldState.from_scenario(generated_scenario)
    objects = OfflineSimulationController(world, _ObjectSink(MetricsRecorder()))
    TickEngine(world, phase_handler=objects).run(steps=20)
    assert made and len(objects.artifacts.proposals) == len(made)


class _ObjectSink(_MetricSink):
    def __init__(self, metrics: MetricsRecorder) -> None:
        super().__init__(metrics)
        self.proposals: list[Any] = []

    def record_decision(self, proposal: Any, *, observed_state: Any = None) -> None:
        self.proposals.append(proposal)


def test_a_row_answers_as_its_proposal_and_makes_it_once(generated_scenario: Any) -> None:
    world, controller, _sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    # The same proposals as Python objects (rules/behavior.rs).
    made_objects = native.rules("behavior").propose_due(controller, view, CounterRNG(world.seed))
    row = rows[0]
    proposal = made_objects[0]
    assert not row.has_object
    for name in (
        "agent_id",
        "action",
        "next_decision_at",
        "target_id",
        "minute_index",
        "observed_state_hash",
    ):
        assert getattr(row, name) == getattr(proposal, name)
    assert row.action is proposal.action
    assert not row.has_object
    assert row.candidates == proposal.candidates
    assert row.has_object
    made = row.materialize()
    assert type(made) is ActionProposal and made == proposal
    assert row.materialize() is made
    assert row.with_outcome("X", 10**6) == proposal.with_outcome("X", 10**6)
    with pytest.raises(AttributeError):
        row.nothing_of_the_kind  # noqa: B018


def test_an_observed_row_reads_as_the_snapshot(generated_scenario: Any) -> None:
    world, controller, _sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    observed = _rules().observed_row(controller, view, rows[0].agent_id)
    expected = native.rules("behavior").observed_state(controller, view, rows[0].agent_id)
    assert type(expected) is dict
    assert _exact(dict(observed)) == _exact(expected)
    assert observed["agent_id"] == rows[0].agent_id


def test_a_sink_in_python_gets_a_dict_and_a_silent_one_nothing(generated_scenario: Any) -> None:
    """A sink that declares no `decision_sink` of its own: its `record_decision` keeps the
    record, the same one the kernel writes as bytes for the indexed sink."""

    class Other(_IndexedMetricSink):
        pass

    world, controller, sink = _indexed_controller(generated_scenario)
    other = Other(MetricsRecorder(), world)
    other_controller = OfflineSimulationController(world, other)
    view, rows = _step_until_due(world, controller)
    observed = _rules().observed_row(other_controller, view, rows[0].agent_id)
    assert type(observed) is dict
    _record(other_controller, rows[0], observed, committed=False)
    _record(controller, rows[0], _rules().observed_row(controller, view, rows[0].agent_id))
    assert type(other.observations[-1]) is dict and type(sink.observations[-1]) is bytes
    assert _exact(other.observations[-1]) == _exact(json.loads(sink.observations[-1]))

    silent = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    assert type(_rules().observed_row(silent, view, rows[0].agent_id)) is not dict
    _record(silent, rows[0], None, committed=False)
    assert world.model_state["agent_dynamics_v1"]["agents"][rows[0].agent_id]["last_action"] == (
        rows[0].action.value
    )


def test_a_view_python_made_is_refused(generated_scenario: Any) -> None:
    world, controller, _sink = _indexed_controller(generated_scenario)
    _step_until_due(world, controller)
    # A copy of the kernel's view, without its snapshot.
    view = replace(view_of(world))
    assert "_rust_view" not in view.__dict__
    with pytest.raises(native.Unsupported, match="Python's"):
        _rules().propose_rows(controller, view, CounterRNG(1))
    with pytest.raises(native.Unsupported, match="Python's"):
        _rules().observed_row(controller, view, "A001")
    with pytest.raises(KernelRefusal, match="Python's"):
        controller._propose_due(view, CounterRNG(1))


def test_what_the_rows_do_not_take_goes_to_the_sink_s_own_record(generated_scenario: Any) -> None:
    """A proposal Python made, an observed state of a dict, a policy's source the sink keeps as
    another type: the sink's `record_decision` writes what the kernel would have, as a dict."""
    world, controller, sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    row = rows[0]
    observed = _rules().observed_row(controller, view, row.agent_id)
    _record(controller, row, observed, committed=False)
    written = json.loads(sink.observations[-1])
    for proposal, state in ((row.materialize(), observed), (row, dict(observed))):
        _record(controller, proposal, state, committed=False)
        assert type(sink.observations[-1]) is dict
        assert _exact(sink.observations[-1]) == _exact(written)
    sink.policy_sources[row.policy_version] = 1
    _record(controller, row, observed, committed=False)
    assert sink.observations[-1]["source_id"] == 1
    assert len(sink.observations) == 4


def test_a_next_decision_not_after_the_minute_is_with_outcome_s_error(
    generated_scenario: Any,
) -> None:
    world, controller, sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    row = rows[0]
    observed = _rules().observed_row(controller, view, row.agent_id)
    before = len(world.event_log)
    with pytest.raises(ValueError, match="invalid decision timing"):
        controller._record_outcome(
            row,
            target_id=None,
            next_decision_at=row.minute_index,
            committed=True,
            observed_state=observed,
        )
    assert sink.observations == [] and len(world.event_log) == before
    controller._record_outcome(
        row,
        target_id=None,
        next_decision_at=row.next_decision_at,
        committed=True,
        observed_state=observed,
    )
    assert type(sink.observations[-1]) is bytes


@pytest.mark.parametrize(
    "presence",
    [
        {"people": {"@": {"household_id": "H"}}},
        {"people": {"@": {"household_id": None}}},
        {"people": {"@": {}}},
        {"people": {}},
        {},
        {"people": {"@": "not a dict"}},
        "not a dict",
    ],
)
def test_the_household_is_the_python_rules(generated_scenario: Any, presence: Any) -> None:
    world, controller, sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    row = rows[0]
    text = json.dumps(presence).replace("@", row.agent_id)
    world.model_state["population_presence_v1"] = json.loads(text)
    _record(controller, row, _rules().observed_row(controller, view, row.agent_id))
    assert json.loads(sink.observations[-1])["household_id"] == current_household_id(
        world, row.agent_id
    )


@pytest.mark.parametrize(
    "presence", [{"people": ["a list"]}, {"people": {"@": {"household_id": 3}}}]
)
def test_a_household_no_rule_reads_is_a_kernel_refusal(
    generated_scenario: Any, presence: Any
) -> None:
    """A presence record of another type than its rows hold: the rows leave it to the sink, whose
    `current_household_id` (`kernel_only`) refuses it before anything is recorded."""
    world, controller, sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    row = rows[0]
    text = json.dumps(presence).replace("@", row.agent_id)
    world.model_state["population_presence_v1"] = json.loads(text)
    with pytest.raises(KernelRefusal, match="current_household_id"):
        _record(controller, row, _rules().observed_row(controller, view, row.agent_id))
    assert sink.observations == []


@pytest.mark.parametrize(("size", "status"), [(9000, "recorded"), (20000, "omitted_size_limit")])
def test_a_long_observed_state_is_measured_by_the_sink(
    generated_scenario: Any, size: int, status: str
) -> None:
    world, controller, sink = _indexed_controller(generated_scenario)
    _step_until_due(world, controller)
    agent = next(iter(world.agents.values()))
    agent.target_id = "x" * size
    view = native.rules("world_view").view_for_proposals(controller, world, world.minute_index)
    rows = _rules().propose_rows(controller, view, CounterRNG(world.seed))
    assert rows
    row = rows[0]
    _record(controller, row, _rules().observed_row(controller, view, agent.id))
    assert sink.observations[-1]["observed_state_status"] == status


def test_a_lifecycle_kernel_refusal_after_the_row_is_a_kernel_refusal(
    generated_scenario: Any,
) -> None:
    """An event counter no Run reaches: the lifecycle's rule (`kernel_only`) refuses it, with no
    receipt, after the row."""
    world, controller, sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    row = rows[0]
    observed = _rules().observed_row(controller, view, row.agent_id)
    world.id_counters["event"] = 2**62 + 1
    before = len(world.event_log)
    with pytest.raises(KernelRefusal):
        _record(controller, row, observed)
    assert len(sink.observations) == 1 and type(sink.observations[0]) is bytes
    assert world.id_counters["event"] == 2**62 + 1
    assert len(world.event_log) == before


def test_a_memory_kernel_refusal_after_the_row_is_a_kernel_refusal(
    generated_scenario: Any,
) -> None:
    """A record no rule writes: the memory's rule (`kernel_only`) refuses it, after the row."""
    world, controller, sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    row = rows[0]
    observed = _rules().observed_row(controller, view, row.agent_id)
    record = world.model_state["agent_dynamics_v1"]["agents"][row.agent_id]
    record["last_decision_minute"] = "not a minute"
    assert world._core.mirror("agent_dynamics_v1").tainted is not None
    with pytest.raises(KernelRefusal):
        _record(controller, row, observed)
    assert len(sink.observations) == 1
    assert record["last_decision_minute"] == "not a minute"


def test_an_uncommitted_decision_opens_nothing(generated_scenario: Any) -> None:
    world, controller, sink = _indexed_controller(generated_scenario)
    view, rows = _step_until_due(world, controller)
    row = rows[0]
    observed = _rules().observed_row(controller, view, row.agent_id)
    before = len(world.event_log)
    _record(controller, row, observed, committed=False)
    assert len(world.event_log) == before and len(sink.observations) == 1


@pytest.mark.parametrize(
    ("level", "digest"),
    [(TraceLevel.SUMMARY, "ebcad2fa3d47d1cd"), (TraceLevel.FULL, "2553c70c92c803d8")],
)
def test_a_trace_keeps_the_documents_of_python(
    generated_scenario: Any, tmp_path: Path, level: TraceLevel, digest: str
) -> None:
    document = copy.deepcopy(generated_scenario.model_dump(mode="json"))
    document["duration_minutes"] = 60
    run_scenario(ScenarioSpec.model_validate(document), tmp_path, trace_level=level)
    written = (tmp_path / "decisions.jsonl").read_bytes()
    assert written
    _python_s(written.decode("utf-8"), digest)


def test_an_index_reads_a_row_of_bytes_as_its_dict(
    generated_scenario: Any, tmp_path: Path, saved_timeline: Any
) -> None:
    store, timeline = saved_timeline(tmp_path, generated_scenario)
    timeline.advance("main", 30, checkpoint_interval=15)
    index = HistoryIndex(store, "p", "r")
    with index.connect() as db:
        rows = db.execute(
            f"SELECT k.name, {history_index._R_KEY} AS key, r.minute, "
            + ", ".join(
                f"(SELECT name FROM names WHERE id=r.{column})"
                for column in ("event", "cause", "source", "type", "status")
            )
            + ", r.branch, r.kind, r.position, r.payload, r.payload_hash "
            f"FROM records r JOIN kinds k ON k.code=r.kind ORDER BY k.name, {history_index._R_KEY}"
        ).fetchall()
        # Payloads compare as text: compressed bytes may differ between zlib builds, and a
        # row made from an EventLog record is made again from it (#138).
        records = [
            [*tuple(row)[:8], text]
            for row, text in zip(rows, index.payload_texts(db, rows), strict=True)
        ]
    assert records and any(row[0] == "actions" for row in records)
    _python_s(records, "8c860d77903353f7")

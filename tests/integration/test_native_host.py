"""The Run's policies and the native Host (#152, lane P8-0b).

Each controller's `RunCore` (`native/src/core/run.rs`) reads, when the controller starts, what the
kernels read of its policies and managers (`sheltersim_core::run::Policies`); `refresh_policies`
reads them again after a test puts another one on the controller. Reading never raises: an
attribute whose read raised is raised again where it is used (policy E14).

The native `Host` (`native/src/host.rs`) is how the core's rules reach Python: the whole world's
validation, the pydantic contracts of inputs (policy E15), the LLM boundary and the outside sinks.
Each method calls the Python the kernels call today; `sheltersim_native.host_call` calls one by
name for these tests.

Lane P9-0 adds what the core's dispatch and tick will ask (design-p9.md §4): what the world holds
(`holding`, read again after a call that runs Python), the configs' sections the host fills
(`fill_section`), the dispatcher's handlers in the Run's policies, a foreign handler
(`apply_foreign`), a decision a Python sink records (`record_decision_python`) and the clock's
error.
"""

from __future__ import annotations

import copy
import json
from types import SimpleNamespace
from typing import Any

import pytest
import sheltersim_native
from pydantic import ValidationError
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core.engine import TickEngine, TickExecutionError, TickPhase
from sheltersim.core.rng import CounterRNG
from sheltersim.core.state_mirror import OWNED_FIELDS, OWNED_NAMESPACES
from sheltersim.core.world import WorldState
from sheltersim.events import handlers as event_handlers
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventLogRecord,
    EventStatus,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _IndexedMetricSink, _MetricSink
from sheltersim.inspection.traces import TraceLevel
from sheltersim.llm.runtime import SocialRuntimeLimits


def _world() -> WorldState:
    scenario = ScenarioSpec.model_validate(json.loads(SCENARIO_100_AGENTS.read_text()))
    return WorldState.from_scenario(scenario)


def _controller(artifacts: Any = None) -> OfflineSimulationController:
    return OfflineSimulationController(_world(), artifacts or _MetricSink(MetricsRecorder()))


def _call(controller: Any, method: str, *arguments: Any, **options: Any) -> Any:
    return sheltersim_native.host_call(controller, method, arguments, **options)


class _Recording:
    """A stand-in that writes down each method called on it, with its arguments."""

    def __init__(self, **attributes: Any) -> None:
        self.calls: list[tuple[str, tuple[Any, ...], dict[str, Any]]] = []
        self.__dict__.update(attributes)

    def __getattr__(self, name: str) -> Any:
        if name.startswith("_"):
            raise AttributeError(name)

        def record(*arguments: Any, **options: Any) -> None:
            self.calls.append((name, arguments, options))

        return record


def test_a_controller_reads_its_policies_once_when_it_starts() -> None:
    controller = _controller()
    summary = controller._run_core.policies_summary()
    # The dispatcher's part (lane P9-0) has its own test below.
    assert summary.pop("dispatch_known") == "true"
    assert summary.pop("dispatch")[0] == str(len(controller.event_dispatcher.handlers))
    assert summary == {
        "controller": True,
        "psychology": "Simple",
        "behavior": "Utility",
        "stress": "Sparse",
        "information": "None",
        "proximity": "false",
        "proximity_config": "false",
        "complaint_wait_minutes": "None",
        "toilet_service": "true",
        "resource_service": "false",
        "staff_service": "true",
        "decision_sink": "Silent",
        "trace_level": "Off",
        "llm": "Llm { router: false, social_policy: false }",
    }
    # Another policy put on the controller is read at `refresh_policies`, not before.
    controller.social_policy = SimpleNamespace(
        limits=SocialRuntimeLimits(complaint_wait_minutes=45)
    )
    controller.artifacts = _IndexedMetricSink(MetricsRecorder(), controller.world)
    assert controller._run_core.policies_summary()["complaint_wait_minutes"] == "None"
    controller._run_core.refresh_policies(controller)
    summary = controller._run_core.policies_summary()
    assert summary["complaint_wait_minutes"] == "Some(45)"
    assert summary["decision_sink"] == "Indexed"
    assert summary["llm"] == "Llm { router: false, social_policy: true }"


def test_policies_are_not_state_and_a_tick_leaves_them() -> None:
    controller = _controller()
    before = controller._run_core.policies_summary()
    engine = TickEngine(controller.world, phase_handler=controller)
    engine.step()
    engine.step()
    assert controller.world.minute_index == 2
    assert controller._run_core.policies_summary() == before


def test_what_a_read_raised_is_raised_where_it_is_used() -> None:
    controller = _controller()
    del controller.llm_router
    del controller.artifacts
    controller._run_core.refresh_policies(controller)
    summary = controller._run_core.policies_summary()
    assert summary["llm"].startswith('! Other("builtins.AttributeError")')
    # No artifacts: decisions are recorded in Python, as `sink_of` says.
    assert summary["decision_sink"] == "Python"
    with pytest.raises(AttributeError, match="llm_router"):
        _call(controller, "llm")
    with pytest.raises(AttributeError, match="artifacts"):
        _call(controller, "trace_level")
    # A trace level no kernel knows is the read site's refusal.
    controller.artifacts = SimpleNamespace(trace_level="verbose")
    controller.llm_router = None
    controller._run_core.refresh_policies(controller)
    with pytest.raises(native.Unsupported, match="a trace level this kernel does not know"):
        _call(controller, "trace_level")
    # A Run made without a controller refuses every policy.
    bare = native.run_core({})
    assert bare.policies_summary()["controller"] is False
    controller._run_core = bare
    with pytest.raises(native.Unsupported, match="the run has no controller"):
        _call(controller, "llm")


def test_the_host_validates_and_runs_contracts_as_python_does() -> None:
    controller = _controller()
    assert _call(controller, "validate_world") is None
    assert _call(controller, "validate_extensions") is None
    assert _call(controller, "contract", "set_staff_role", {"skills": ["medic"]}) == {
        "skills": ["medic"]
    }
    with pytest.raises(ValidationError):
        _call(controller, "contract", "set_staff_role", {"skills": []})
    with pytest.raises(KeyError):
        _call(controller, "contract", "no_such_operator", {})


def test_the_llm_boundary_calls_the_social_policy_and_the_controller() -> None:
    controller = _controller()
    policy = _Recording()
    controller.social_policy = policy
    _call(controller, "consider_support_request", "agent-1", "request-1")
    _call(controller, "consider_complaint_response", "agent-2", "request-2")
    _call(controller, "consider_cooperation", "agent-1", "agent-2", "event-1")
    _call(controller, "consider_information_reaction", "agent-2", "agent-1", "message-1", "event-2")
    world = controller.world
    assert policy.calls == [
        ("consider_support_request", (world,), {"agent_id": "agent-1", "request_id": "request-1"}),
        (
            "consider_complaint_response",
            (world,),
            {"agent_id": "agent-2", "request_id": "request-2"},
        ),
        (
            "consider_cooperation",
            (world,),
            {
                "initiator_agent_id": "agent-1",
                "participant_agent_id": "agent-2",
                "trigger_event_id": "event-1",
            },
        ),
        (
            "consider_information_reaction",
            (world,),
            {
                "agent_id": "agent-2",
                "participant_agent_id": "agent-1",
                "message_id": "message-1",
                "trigger_event_id": "event-2",
            },
        ),
    ]
    asked: list[tuple[str, Any]] = []
    controller._prioritize_support_request = lambda request_id, proposal: asked.append(
        (request_id, proposal)
    )
    _call(controller, "prioritize_support_request", "request-1", "agent-1", "a" * 64)
    assert asked == [
        ("request-1", SimpleNamespace(agent_id="agent-1", observed_state_hash="a" * 64))
    ]


def test_the_metric_sink_takes_the_cores_sample_as_the_recorder_would() -> None:
    recorded = _controller()
    expected = MetricsRecorder()
    expected.capture(recorded.world, minute=0)
    _call(recorded, "capture_metric", 0)
    assert recorded.artifacts.metrics.snapshots == expected.snapshots
    # Any other sink gets `capture_metric(world, minute=)`.
    sink = _Recording()
    recorded.artifacts = sink
    _call(recorded, "capture_metric", 0)
    assert sink.calls == [("capture_metric", (recorded.world,), {"minute": 0})]


class _Writer(_Recording):
    decision_sink = "artifacts"


def test_decisions_and_traces_reach_the_sink() -> None:
    controller = _controller()
    indexed = _IndexedMetricSink(MetricsRecorder(), controller.world)
    controller.artifacts = indexed
    controller._run_core.refresh_policies(controller)
    assert _call(controller, "decision_sink") == "Indexed"
    _call(controller, "decision", b'{"agent_id":"a"}')
    assert indexed.observations == [b'{"agent_id":"a"}']

    writer = _Writer(trace_level=TraceLevel.FULL, _started=False, _finalized=False)
    controller.artifacts = writer
    controller._run_core.refresh_policies(controller)
    # Before the writer starts, its own `record_decision` runs in Python.
    assert _call(controller, "decision_sink") == "Python"
    writer._started = True
    assert _call(controller, "decision_sink") == "Artifacts"
    assert _call(controller, "trace_level") == "Full"
    _call(controller, "decision", {"action": "rest"}, {"stress": 0.5})
    _call(controller, "trace", "psychology", {"minute": 1}, [{"agent_id": "a"}])
    _call(controller, "trace", "social_contact", {"minute": 2})
    _call(controller, "trace", "information_spread", {"minute": 3})
    assert writer.calls == [
        ("record_decision_document", ({"action": "rest"}, {"stress": 0.5}), {}),
        ("record_psychology_trace", ({"minute": 1},), {"agent_contributions": [{"agent_id": "a"}]}),
        ("record_social_contact_trace", ({"minute": 2},), {}),
        ("record_information_spread_trace", ({"minute": 3},), {}),
    ]
    off = _Writer(trace_level=TraceLevel.OFF, _started=True, _finalized=False)
    controller.artifacts = off
    controller._run_core.refresh_policies(controller)
    assert _call(controller, "decision_sink") == "Silent"


def test_the_observer_the_space_and_a_test_draw() -> None:
    controller = _controller()
    seen: list[tuple[Any, bool]] = []
    _call(
        controller,
        "phase",
        "update_agent_state",
        True,
        observer=lambda phase, before: seen.append((phase, before)),
    )
    assert seen == [(TickPhase.UPDATE_AGENT_STATE, True)]
    assert seen[0][0] is TickPhase.UPDATE_AGENT_STATE
    # Without an observer nothing is called.
    assert _call(controller, "phase", "collect_metrics", False) is None
    epoch = controller._run_core.epoch
    _call(controller, "space_refreshed")
    assert controller._run_core.epoch == epoch + 1
    rng = _Recording()
    rng.random = lambda *parts: (rng.calls.append(("random", parts, {})), 0.25)[1]
    assert _call(controller, "draw", "toilet_service", "agent-1", 5, rng=rng) == 0.25
    assert rng.calls == [("random", ("toilet_service", "agent-1", 5), {})]
    rng.random = lambda *parts: 1
    with pytest.raises(native.Unsupported, match="a draw that is not a float"):
        _call(controller, "draw", "x", rng=rng)
    with pytest.raises(native.Unsupported, match="the host has no random source"):
        _call(controller, "draw", "x")


# ---- Lane P9-0: the host kit ---------------------------------------------------------------------


def _calls(controller: Any, *calls: tuple[str, tuple[Any, ...]]) -> list[Any]:
    """Several methods of one host, in turn: it keeps what it read between them."""
    return sheltersim_native.host_calls(controller, list(calls))


def _handlers(controller: Any) -> dict[str, str]:
    return controller._run_core.policies_summary()["dispatch"][1]


def test_the_run_reads_the_dispatchers_handlers(monkeypatch: pytest.MonkeyPatch) -> None:
    controller = _controller()
    handlers = _handlers(controller)
    assert handlers.keys() == controller.event_dispatcher.handlers.keys()
    assert handlers["social_interaction"] == "SocialInteraction own_managers=true arrival=true"
    assert handlers["toilet_service_complete"] == (
        'ToiletServiceCompletion after="own" own_managers=true arrival=true'
    )
    assert handlers["staff_support_complete"] == (
        "StaffSupportCompletion cooldown=30 colocated=true own_managers=true arrival=true"
    )
    assert handlers["support_request_timeout"] == (
        "SupportRequestTimeout cooldown=30 own_managers=true arrival=true"
    )
    assert handlers["effect_program_expire"] == (
        "EffectProgramFacility expire=true own_managers=true arrival=true"
    )
    assert handlers["effect_program_apply"] == (
        "EffectProgramFacility expire=false own_managers=true arrival=true"
    )
    assert handlers["resource_reserve"] == (
        'ResourceLedger operation="reserve" own_managers=true arrival=true'
    )
    assert handlers["agent_travel_complete"] == (
        'AgentTravelCompletion toilet_service="as_defined" resource_service="as_defined"'
        " own_managers=true arrival=true"
    )
    assert set(handlers.values()) - {"foreign"} == set(handlers.values())

    # What a test puts on the dispatcher is read at `refresh_policies`, not before.
    class Probe:
        def apply(self, world: Any, event: Any) -> None:
            return None

    controller.event_dispatcher.handlers["probe"] = Probe()
    controller.toilet_service.request = lambda *arguments, **options: None
    controller.event_dispatcher.handlers["toilet_service_complete"] = (
        event_handlers.ToiletServiceCompletionHandler(after_complete=None)
    )
    assert "probe" not in _handlers(controller)
    controller._run_core.refresh_policies(controller)
    handlers = _handlers(controller)
    assert handlers["probe"] == "foreign"
    assert handlers["agent_travel_complete"].startswith(
        'AgentTravelCompletion toilet_service="own_request"'
    )
    assert handlers["toilet_service_complete"].startswith('ToiletServiceCompletion after="none"')
    # A patched `apply` makes the class's handlers foreign; a patched `_after_arrival` is read.
    monkeypatch.setattr(
        event_handlers.SocialInteractionHandler, "apply", lambda self, world, event: None
    )
    monkeypatch.setattr(
        event_handlers.AgentTravelCompletionHandler,
        "_after_arrival",
        lambda self, *arguments: None,
    )
    controller._run_core.refresh_dispatch(controller)
    handlers = _handlers(controller)
    assert handlers["social_interaction"] == "foreign"
    assert handlers["agent_travel_complete"].endswith("arrival=false")
    # Another manager than the controller's.
    controller.event_dispatcher.handlers["agent_travel_complete"] = (
        event_handlers.AgentTravelCompletionHandler(
            controller.toilet_service.__class__(controller.world)
        )
    )
    controller._run_core.refresh_dispatch(controller)
    assert "own_managers=false" in _handlers(controller)["agent_travel_complete"]


def test_a_run_before_its_dispatcher_refuses_to_read_it() -> None:
    controller = _controller()
    bare = native.run_core({}, SimpleNamespace(world=controller.world))
    summary = bare.policies_summary()
    assert summary["dispatch_known"].startswith('! Other("builtins.AttributeError")')
    assert summary["dispatch"] == (summary["dispatch_known"], {})


def _generated(scenario: Any) -> OfflineSimulationController:
    return OfflineSimulationController(
        WorldState.from_scenario(scenario), _MetricSink(MetricsRecorder())
    )


def _new_stamp(world: WorldState) -> None:
    """A write that changes nothing gives the configs a new stamp, with nothing parsed."""
    key = next(iter(world.model_configs))
    world.model_configs[key] = copy.deepcopy(world.model_configs[key])


def test_the_holding_answers_as_each_read_does(generated_scenario: Any) -> None:
    controller = _generated(generated_scenario)
    world = controller.world
    world.model_state["probe_runtime_v1"] = {"schema_version": 1}
    world.model_state["probe_none_v1"] = None
    names = [*OWNED_NAMESPACES, "probe_runtime_v1", "probe_none_v1", "absent_v1"]
    fields = [*OWNED_FIELDS, "not_a_field"]

    def check() -> dict[str, Any]:
        answers = sheltersim_native.holding_and_reads(world, names, fields)
        assert {name: kept for name, (kept, _) in answers.items()} == {
            name: now for name, (_, now) in answers.items()
        }
        return answers

    answers = check()
    held = [name for name in OWNED_NAMESPACES if name in world.model_state]
    assert held
    assert answers[held[0]][0] == ("held", "false")
    assert answers["absent_v1"][0] == ("absent", "true")
    assert answers["probe_none_v1"][0][1] == "true"
    assert answers["metric_state"][0] is True
    # A field assigned whole is no longer its slot's tree.
    world.metric_state = dict(world.metric_state)
    assert check()["metric_state"][0] is False


def test_a_host_call_that_runs_python_makes_the_host_read_the_holding_again() -> None:
    controller = _controller()
    name = "probe_runtime_v1"

    class Policy:
        def consider_cooperation(self, world: Any, **options: Any) -> None:
            # An LLM hook writes model_state, as the social runtime does.
            world.model_state[name] = {"schema_version": 1}

    controller.social_policy = Policy()
    answers = _calls(
        controller,
        ("holding", (name,)),
        ("llm", ()),
        ("holding", (name,)),
        ("holding_reads", ()),
        ("consider_cooperation", ("A001", "A002", "E1")),
        ("holding", (name,)),
        ("holding_reads", ()),
    )
    assert answers[0] == ("absent", "true", True)
    # A call that runs no Python keeps what was read.
    assert answers[2:4] == [("absent", "true", True), 1]
    # The hook wrote the namespace: the next read sees it.
    assert answers[5:] == [("held", "false", True), 2]


_SECTIONS = [
    "selection",
    "tick_plan",
    "facility_geometry",
    "spatial_constraints",
    "staff_fatigue",
    "staff_operations",
    "staff_operations_known",
    "support_task_minutes",
    "support_cooldown_minutes",
    "information_spread",
    "body_models",
    "social_interaction_minutes",
    "care_outcome",
    "agent_dynamics_policy",
    "facility_awareness",
    "knowledge_is_limited",
    "zone_environment",
    "infrastructure",
    "proximity",
    "movement",
    "route_capacities",
    "transit_capacities",
    "facility_queues",
    "overlay_schema",
    "overlay_rooms",
    "resource_service",
    "accessibility_needs",
    "extensions_enabled",
]


def test_the_host_fills_each_section_as_its_read_site_parses_it(generated_scenario: Any) -> None:
    controller = _generated(generated_scenario)
    world = controller.world
    _new_stamp(world)
    for name in _SECTIONS:
        before = sheltersim_native.config_parses()
        assert _calls(controller, ("fill_section", (name,))) == [("kept", True, True)], name
        parsed = sheltersim_native.config_parses() - before
        # Parsed once, where the read site's accessor counts it; asked again, nothing is parsed.
        assert parsed <= 1, name
        assert _calls(controller, ("fill_section", (name,))) == [("kept", True, True)], name
        assert sheltersim_native.config_parses() == before + parsed, name
    # The read site finds the section filled: it does not parse it again.
    before = sheltersim_native.config_parses()
    sheltersim_native.config_section(world, "movement")
    sheltersim_native.config_section(world, "staff_operations")
    assert sheltersim_native.config_parses() == before
    # Configs no slot follows serve one read: each fill parses.
    world.model_configs = dict(world.model_configs)
    before = sheltersim_native.config_parses()
    assert _calls(controller, ("fill_section", ("movement",))) == [("kept", True, True)]
    assert _calls(controller, ("fill_section", ("movement",))) == [("kept", True, True)]
    assert sheltersim_native.config_parses() == before + 2


def test_a_patched_selection_is_filled_for_one_read(monkeypatch: pytest.MonkeyPatch) -> None:
    from sheltersim.contracts import policies

    controller = _controller()
    _new_stamp(controller.world)
    contract = policies.selected_models
    monkeypatch.setattr(policies, "selected_models", lambda configs: contract(configs))
    assert _calls(controller, ("fill_section", ("selection",))) == [("once", True, False)]


def _probe_event(event_type: str = "probe") -> ScheduledEvent:
    return ScheduledEvent(
        event_id="E-probe",
        kind=EventKind.EXOGENOUS,
        due_minute=0,
        priority=0,
        sequence=1,
        event_type=event_type,
        payload={"agent_id": "A001"},
        source_id="probe-1",
    )


def test_a_foreign_handler_runs_through_the_host() -> None:
    controller = _controller()
    seen: list[Any] = []
    follow = _probe_event("probe_follow")

    class Probe:
        outcome: BaseException | None = None

        def apply(self, world: Any, event: Any) -> Any:
            seen.append((world, event.to_dict()))
            if self.outcome is not None:
                raise self.outcome
            return (EventLogRecord(follow, 0, EventStatus.APPLIED, "done"),)

    probe = Probe()
    controller.event_dispatcher.handlers["probe"] = probe
    event = _probe_event()
    assert _calls(controller, ("apply_foreign", (event,))) == [
        ("applied", None, [("E-probe", "applied", "done", 0)])
    ]
    assert seen == [(controller.world, event.to_dict())]
    probe.outcome = EventSuppressed("not now")
    assert _calls(controller, ("apply_foreign", (event,))) == [("suppressed", "not now", [])]
    probe.outcome = EventApplicationError("broken")
    assert _calls(controller, ("apply_foreign", (event,))) == [("failed", "broken", [])]
    probe.outcome = RuntimeError("other")
    with pytest.raises(RuntimeError, match="other"):
        _calls(controller, ("apply_foreign", (event,)))
    with pytest.raises(native.Unsupported, match="no handler is registered"):
        _calls(controller, ("apply_foreign", (_probe_event("unregistered"),)))


def test_a_python_sink_records_the_decision_as_the_step_did() -> None:
    world = WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    rules = native.rules("decisions")
    while True:
        engine.step()
        view = native.rules("world_view").view_for_proposals(controller, world, world.minute_index)
        rows = rules.propose_rows(controller, view, CounterRNG(world.seed))
        if rows:
            break
    row = rows[0]
    sink = _Recording()
    controller.artifacts = sink
    observed = native.rules("behavior").observed_state(controller, view, row.agent_id)
    target = row.target_id
    _calls(controller, ("record_decision_python", (row, target, row.next_decision_at, object())))
    _calls(controller, ("record_decision_python", (row, None, row.next_decision_at + 1, {"x": 1})))
    assert sink.calls == [
        (
            "record_decision",
            (row.with_outcome(target, row.next_decision_at),),
            {"observed_state": observed},
        ),
        (
            "record_decision",
            (row.with_outcome(None, row.next_decision_at + 1),),
            {"observed_state": {"x": 1}},
        ),
    ]


def test_the_clock_error_is_the_engines() -> None:
    with pytest.raises(
        TickExecutionError, match="^tick phase attempted to change the engine-owned clock$"
    ):
        _call(_controller(), "clock_error")

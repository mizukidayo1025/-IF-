"""The Run's policies and the native Host (#152, lane P8-0b).

Each controller's `RunCore` (`native/src/core/run.rs`) reads, when the controller starts, what the
kernels read of its policies and managers (`sheltersim_core::run::Policies`); `refresh_policies`
reads them again after a test puts another one on the controller. Reading never raises: an
attribute whose read raised is raised again where it is used (policy E14).

The native `Host` (`native/src/host.rs`) is how the core's rules reach Python: the whole world's
validation, the pydantic contracts of inputs (policy E15), the LLM boundary and the outside sinks.
Each method calls the Python the kernels call today; `sheltersim_native.host_call` calls one by
name for these tests.
"""

from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any

import pytest
import sheltersim_native
from pydantic import ValidationError
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core.engine import TickEngine, TickPhase
from sheltersim.core.world import WorldState
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
    assert controller._run_core.policies_summary() == {
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

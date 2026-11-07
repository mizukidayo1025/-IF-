"""Boundary and censoring semantics for online high-distress episode counts.

The monitor is updated by the update phase's `_update_long_term_psychology`, whose rule is
native/src/rules/psychology.rs (the Python record_distress_monitor_tick was deleted in lane L10):
each test runs that step on a small world, one minute after another, and reads the summary.
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

import pytest

from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.distress_monitor import distress_monitor_summary
from sheltersim.domain.models import AgentProfile, AgentState, Mobility
from sheltersim.domain.psychological_state import (
    PSYCHOLOGICAL_STATE_KEY,
    create_psychological_state,
)
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.policies.psychology import MeanRevertingDistressPolicy, PsychologyParameters


class _Monitor:
    """A world whose people have the previous distress, and the step with the current one."""

    def __init__(self, threshold: float) -> None:
        self.world = WorldState(
            "s", 1, 100, model_state={PSYCHOLOGICAL_STATE_KEY: create_psychological_state({})}
        )
        self.controller = SimpleNamespace(
            psychology=MeanRevertingDistressPolicy(
                PsychologyParameters(high_distress_threshold=threshold)
            )
        )
        self.minute = 0

    @property
    def metrics(self) -> Any:
        return self.world.metric_state

    def tick(self, *, previous_by_agent: dict[str, Any], current_by_agent: dict[str, Any]) -> None:
        for agent_id, stress in previous_by_agent.items():
            if agent_id not in self.world.agents:
                self.world.agents[agent_id] = AgentState(
                    AgentProfile(agent_id, None, "z", Mobility.INDEPENDENT, 0.0, 1.0, agent_id), "z"
                )
            self.world.agents[agent_id].stress = stress
        deltas = tuple(
            SimpleNamespace(agent_id=agent_id, stress=stress)
            for agent_id, stress in current_by_agent.items()
        )
        OfflineSimulationController._update_long_term_psychology(
            self.controller, self.world, None, deltas, self.minute
        )
        self.minute += 1


def test_threshold_boundary_recovery_reascent_and_right_censoring_are_separate() -> None:
    monitor = _Monitor(0.7)
    monitor.tick(
        previous_by_agent={"A001": 0.69, "A002": 0.2}, current_by_agent={"A001": 0.70, "A002": 0.2}
    )
    monitor.tick(
        previous_by_agent={"A001": 0.70, "A002": 0.2}, current_by_agent={"A001": 0.70, "A002": 0.2}
    )
    monitor.tick(
        previous_by_agent={"A001": 0.70, "A002": 0.2}, current_by_agent={"A001": 0.699, "A002": 0.2}
    )
    monitor.tick(
        previous_by_agent={"A001": 0.699, "A002": 0.2}, current_by_agent={"A001": 0.71, "A002": 0.2}
    )

    summary = distress_monitor_summary(monitor.metrics)
    assert summary["high_distress_episode_count"] == 2
    assert summary["completed_high_distress_episode_count"] == 1
    assert summary["ongoing_high_distress_episode_count"] == 1
    assert summary["mean_completed_episode_duration_minutes"] == 2.0
    assert summary["ongoing_episode_elapsed_minutes_by_agent"] == [
        {"agent_id": "A001", "elapsed_minutes": 1}
    ]
    assert summary["ongoing_episode_elapsed_minutes_total"] == 1
    assert summary["longest_high_distress_run_minutes"] == 2
    assert summary["high_distress_reascent_count"] == 1
    assert summary["end_high_distress_agent_share"] == 0.5


def test_empty_monitor_has_defined_zero_summary() -> None:
    summary = distress_monitor_summary({})
    assert summary["high_distress_episode_count"] == 0
    assert summary["completed_high_distress_episode_count"] == 0
    assert summary["ongoing_high_distress_episode_count"] == 0
    assert summary["mean_completed_episode_duration_minutes"] == 0.0
    assert summary["end_high_distress_agent_share"] == 0.0


def test_online_distress_monitor_tracks_high_runs_recovery_and_reascent() -> None:
    monitor = _Monitor(0.7)
    monitor.tick(previous_by_agent={"A001": 0.8}, current_by_agent={"A001": 0.85})
    monitor.tick(previous_by_agent={"A001": 0.85}, current_by_agent={"A001": 0.9})
    monitor.tick(previous_by_agent={"A001": 0.9}, current_by_agent={"A001": 0.3})
    monitor.tick(previous_by_agent={"A001": 0.3}, current_by_agent={"A001": 0.8})
    summary = distress_monitor_summary(monitor.metrics)
    assert summary["high_distress_episode_count"] == 2
    assert summary["high_distress_reascent_count"] == 1
    assert summary["longest_high_distress_run_minutes"] == 2
    assert summary["completed_recovery_count"] == 1
    assert summary["mean_completed_recovery_minutes"] == 2.0


def test_a_fall_below_the_threshold_during_event_handling_closes_the_run() -> None:
    """Staff support can lower distress before the psychology update (#159)."""
    monitor = _Monitor(0.7)
    monitor.tick(previous_by_agent={"A001": 0.8}, current_by_agent={"A001": 0.75})
    monitor.tick(previous_by_agent={"A001": 0.75}, current_by_agent={"A001": 0.72})
    # An event lowered distress to 0.69 between the two updates.
    monitor.tick(previous_by_agent={"A001": 0.69}, current_by_agent={"A001": 0.68})

    summary = distress_monitor_summary(monitor.metrics)
    assert summary["completed_recovery_count"] == 1
    assert summary["mean_completed_episode_duration_minutes"] == 2.0
    assert summary["ongoing_high_distress_episode_count"] == 0


def test_a_fall_during_event_handling_and_a_rise_in_the_same_update_are_two_episodes() -> None:
    """The fall is observed before the update, so the earlier run ends there (#159)."""
    monitor = _Monitor(0.7)
    monitor.tick(previous_by_agent={"A001": 0.8}, current_by_agent={"A001": 0.75})
    # An event lowered distress to 0.69; the update raises it above the threshold again.
    monitor.tick(previous_by_agent={"A001": 0.69}, current_by_agent={"A001": 0.71})

    summary = distress_monitor_summary(monitor.metrics)
    assert summary["high_distress_episode_count"] == 2
    assert summary["completed_recovery_count"] == 1
    assert summary["mean_completed_episode_duration_minutes"] == 1.0
    assert summary["ongoing_episode_elapsed_minutes_by_agent"] == [
        {"agent_id": "A001", "elapsed_minutes": 1}
    ]
    assert summary["high_distress_reascent_count"] == 1


@pytest.mark.parametrize("value", [float("nan"), 1.5, -0.1, True, "0.5"])
def test_values_outside_the_unit_interval_are_refused_and_change_nothing(value) -> None:
    """The deleted rule raised on these; a Run's distress is a float within 0..1."""
    monitor = _Monitor(0.5)
    with pytest.raises(KernelRefusal):
        monitor.tick(previous_by_agent={"A": 0.5}, current_by_agent={"A": value})
    assert dict(monitor.metrics) == {}
    if type(value) is float:
        monitor = _Monitor(0.5)
        with pytest.raises(KernelRefusal):
            monitor.tick(previous_by_agent={"A": value}, current_by_agent={"A": 0.5})
        assert dict(monitor.metrics) == {}


def test_int_distress_and_bad_state_are_refused() -> None:
    """The deleted rule took an int within bounds; the kernels' distress is a float."""
    monitor = _Monitor(0.5)
    with pytest.raises(KernelRefusal):
        monitor.tick(previous_by_agent={"A": 0.0}, current_by_agent={"A": 1})
    monitor.tick(previous_by_agent={"A": 0.0}, current_by_agent={"A": 1.0})
    assert monitor.metrics["acute_distress_monitor:A:current_run"] == 1.0
    monitor.metrics["acute_distress_monitor:A:episodes"] = float("inf")
    with pytest.raises(KernelRefusal):
        monitor.tick(previous_by_agent={"A": 0.0}, current_by_agent={"A": 1.0})
    assert monitor.metrics["acute_distress_monitor:A:current_run"] == 1.0

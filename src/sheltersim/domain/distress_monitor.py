"""Compact online duration indicators for mean-reverting psychology runs.

The update phase's `_update_long_term_psychology` (native/src/rules/psychology.rs) keeps five values
per Agent in checkpointed metric state, under `acute_distress_monitor:<agent_id>:`: the current
and the longest high-distress run, the episodes, and the total and the count of the runs that
ended. A run is the minutes with distress at or above the threshold; one ends when distress was
or is below it, including a fall while events were handled before the update (#159). It keeps no
tick-by-tick history and does not affect model state.
"""

from __future__ import annotations

from collections.abc import Mapping
from typing import Any

_PREFIX = "acute_distress_monitor"


def distress_monitor_summary(metrics: Mapping[str, float]) -> dict[str, Any]:
    """Return aggregate online indicators without loading full psychological traces."""
    agent_keys = sorted(
        key.removesuffix(":current_run")
        for key in metrics
        if key.startswith(f"{_PREFIX}:") and key.endswith(":current_run")
    )
    longest = [metrics.get(f"{key}:longest_run", 0.0) for key in agent_keys]
    episodes = sum(metrics.get(f"{key}:episodes", 0.0) for key in agent_keys)
    recovered_total = sum(metrics.get(f"{key}:recovered_run_total", 0.0) for key in agent_keys)
    recovered_count = sum(metrics.get(f"{key}:recovered_run_count", 0.0) for key in agent_keys)
    current = sum(metrics.get(f"{key}:current_run", 0.0) for key in agent_keys)
    ongoing = [
        {
            "agent_id": key.removeprefix(f"{_PREFIX}:"),
            "elapsed_minutes": int(metrics.get(f"{key}:current_run", 0.0)),
        }
        for key in agent_keys
        if metrics.get(f"{key}:current_run", 0.0) > 0.0
    ]
    mean_completed = recovered_total / recovered_count if recovered_count else 0.0
    return {
        "monitored_agent_count": len(agent_keys),
        "high_distress_episode_count": int(episodes),
        "completed_high_distress_episode_count": int(recovered_count),
        "ongoing_high_distress_episode_count": len(ongoing),
        "ongoing_episode_elapsed_minutes_by_agent": ongoing,
        "ongoing_episode_elapsed_minutes_total": int(current),
        "mean_completed_episode_duration_minutes": mean_completed,
        "high_distress_reascent_count": int(
            sum(max(0.0, metrics.get(f"{key}:episodes", 0.0) - 1.0) for key in agent_keys)
        ),
        "longest_high_distress_run_minutes": int(max(longest, default=0.0)),
        "end_high_distress_agent_share": (len(ongoing) / len(agent_keys) if agent_keys else 0.0),
        "mean_per_agent_longest_high_distress_run_minutes": (
            sum(longest) / len(longest) if longest else 0.0
        ),
        "completed_recovery_count": int(recovered_count),
        # Retained for readers of earlier run artifacts; new reports should use the
        # explicitly episode-scoped mean above.
        "mean_completed_recovery_minutes": mean_completed,
        "currently_high_run_agent_minutes": int(current),
    }

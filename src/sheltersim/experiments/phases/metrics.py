"""The metrics phase: the minute's snapshot, and closing open actions at the end of a Run."""

from __future__ import annotations


class MetricsPhase:
    """TickPhase.COLLECT_METRICS, a mixin of OfflineSimulationController.

    The phase has no step of its own: the driver runs it in native/core/src/rules/tick/metrics.rs.
    `artifacts.capture_metric(world, minute=minute)` (the recorder's sample, or the sink's own
    method when a subclass or a patch changes it), then at the Run's last minute
    `finalize_active_actions(world, minute, active_actions=active_actions)` of
    domain/action_lifecycle.py.
    """

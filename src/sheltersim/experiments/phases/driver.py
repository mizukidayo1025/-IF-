"""The tick's phases in Rust: native/src/rules/tick.rs, which calls the core's `run_tick`
(native/core/src/rules/tick/mod.rs).

`TickEngine.step` asks a handler's `run_tick` first. The driver refuses a tick that a hook changes,
or whose dispatch phase its own checks turn down, before anything has happened, and `step` then runs
the phases through `phase_handler`, so a test's patch sees what it saw before. A trace, a Provider
and an observer of the phases go with the driver. Past those checks each step is its kernel; a
kernel's refusal is KernelRefusal (core/kernels.py). See docs/rust-port/lanes/m6-driver.md.
"""

from __future__ import annotations

from typing import Any

from sheltersim import native
from sheltersim.core.rng import KeyedRandom
from sheltersim.core.world import WorldState
from sheltersim.domain import facilities, resource_service, staff
from sheltersim.experiments import metrics
from sheltersim.experiments.phases import arbitrate, update

# The controller's steps the driver calls no entry of, or that it stands for. A patch of one sends
# the tick through `phase_handler`. A step that the driver calls through Python is not here.
DRIVEN_STEPS = (
    "__call__",
    "_run_phase",
    "_run_dispatch_phase",
    "_run_update_phase",
    "_update_movement",
    "_replan_journeys",
    "_view_after_movement",
    "_psychology_update",
    "_social_propagation",
    "_record_psychology_trace",
    "_update_long_term_psychology",
    "_apply_state_deltas",
    "_run_propose_phase",
    "_view_for_proposals",
    "_apply_due_proposals",
    "_propose_due",
    "_observed_state",
    "_apply_proposal",
    "_apply_effect",
    "_record_outcome",
    "_run_arbitrate_phase",
    "_arbitrate_services",
    "_assign_staff_support",
    "_consider_due_complaints",
    "_run_sample_phase",
    "_sample_information_contacts",
    "_sample_proximity_contacts",
    "_run_metrics_phase",
)

# What those steps call, which the driver calls the kernels of.
DRIVEN_RULES = (
    (update, "advance_movement"),
    (arbitrate, "update_staff_dynamics"),
    (arbitrate, "update_staff_operations"),
    (arbitrate, "assign_staff_operations"),
    (arbitrate, "schedule_staff_operation_relief"),
    (facilities.ToiletServiceManager, "arbitrate"),
    (resource_service.ResourceServiceManager, "arbitrate"),
    (staff.StaffService, "assign_pending"),
    (metrics.MetricsRecorder, "capture"),
    (metrics, "_snapshot_fields"),
    (metrics, "resource_metric_values"),
)


def install_driver(controller: type) -> None:
    """Give this class `run_tick`, once its segments are installed."""
    rules = native.rules("tick")
    rules.install(
        controller,
        (
            facilities.ToiletServiceManager,
            resource_service.ResourceServiceManager,
            staff.StaffService,
        ),
        list(DRIVEN_STEPS),
        list(DRIVEN_RULES),
    )
    run_kernel = rules.run_tick
    unsupported = native.Unsupported

    def run_tick(
        self: Any, world: WorldState, minute: int, rng: KeyedRandom, observer: Any
    ) -> bool:
        """One tick in Rust, or false with nothing changed."""
        self._refresh_config_scopes(world, minute)
        first, second = self._config_scopes()
        with first, second:
            try:
                run_kernel(self, world, minute, rng, observer)
            except unsupported:
                return False
        return True

    controller.run_tick = run_tick  # type: ignore[attr-defined]

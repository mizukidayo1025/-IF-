"""The tick's phases in Rust: native/src/rules/tick.rs, which calls the core's `run_phases`
(native/core/src/rules/tick/mod.rs).

`TickEngine.step` calls a handler's `run_tick`, the only path of a controller's tick (#221). The
driver refuses, before anything has happened, a tick that a hook would change, or whose dispatch
phase its own checks turn down: that is KernelRefusal (core/kernels.py), and the tick does not run.
A patch of a step or a rule below would not be called, so it fails the tick rather than going
unseen. A trace, a Provider and an observer of the phases go with the driver. Past those checks
each step is its kernel; a kernel's refusal is KernelRefusal too. See
docs/rust-port/lanes/m6-driver.md.
"""

from __future__ import annotations

from collections.abc import Iterable
from typing import Any

from sheltersim import native
from sheltersim.core.rng import KeyedRandom
from sheltersim.core.world import WorldState
from sheltersim.domain import facilities, resource_service, staff
from sheltersim.experiments import metrics

# The controller's steps the driver calls no entry of. A patch of one fails the tick. A step that
# the driver calls through Python is not here.
DRIVEN_STEPS = (
    "_run_dispatch_phase",
    "_replan_journeys",
    "_view_after_movement",
    "_psychology_update",
    "_social_propagation",
    "_record_psychology_trace",
    "_update_long_term_psychology",
    "_apply_state_deltas",
    "_view_for_proposals",
    "_propose_due",
    "_observed_state",
    "_apply_effect",
    "_record_outcome",
    "_assign_staff_support",
    "_consider_due_complaints",
    "_sample_information_contacts",
    "_sample_proximity_contacts",
)

# What those steps call, which the driver calls the kernels of.
DRIVEN_RULES = (
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

    def run_tick(
        self: Any,
        world: WorldState,
        minute: int,
        rng: KeyedRandom,
        observer: Any,
        phases: Iterable[str] | None = None,
    ) -> None:
        """One tick in Rust, or KernelRefusal with nothing changed.

        `phases`, members of `TickPhase`, runs only those of the tick, in the tick's order, for a
        test that steps a part of a tick; None runs the whole tick.
        """
        self._refresh_config_scopes(world, minute)
        first, second = self._config_scopes()
        with first, second:
            run_kernel(self, world, minute, rng, observer, None if phases is None else list(phases))

    controller.run_tick = run_tick  # type: ignore[attr-defined]

"""Rust kernels that are steps of a phase, so a lane doesn't edit the phase's steps.

A phase's `_run_*_phase` lists its steps, each a method. A step listed here has no Python body (lane
L10, docs/rust-port/step4-plan.md): the phase's mixin keeps the method's signature and docstring,
which state its contract, and the controller's step is the kernel. A kernel gets the controller and
the step's arguments, and returns what the step returns. Its Unsupported becomes KernelRefusal, as
`kernel_only` of core/kernels.py does for a rule. See docs/rust-port/lanes/p4-world-core.md.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping
from functools import wraps
from typing import Any

from sheltersim import native
from sheltersim.core import kernels

# Name of a step's method -> (the module of native/src/rules/, the name of its kernel).
KERNEL_ONLY_SEGMENTS: dict[str, tuple[str, str]] = {
    "_social_propagation": ("social_propagation", "update_social_propagation"),
    "_sample_information_contacts": ("information", "sample_contacts"),
    "_run_dispatch_phase": ("dispatch", "run_phase"),
    "_assign_staff_support": ("support", "assign_staff_support"),
    "_consider_due_complaints": ("staff", "consider_due_complaints"),
    "_replan_journeys": ("replan", "replan_journeys"),
    "_route": ("travel", "route"),
    "_schedule_travel": ("travel", "schedule_travel"),
    "_schedule_staff_support_travel": ("travel", "schedule_support_travel"),
    "_sample_proximity_contacts": ("proximity", "sample_contacts"),
    "_psychology_update": ("psychology", "update_psychology"),
    "_record_psychology_trace": ("trace", "record_psychology_trace"),
    "_update_long_term_psychology": ("psychology", "update_long_term"),
    "_apply_state_deltas": ("psychology", "apply_deltas"),
    "_view_after_movement": ("world_view", "view_after_movement"),
    "_view_for_proposals": ("world_view", "view_for_proposals"),
    "_propose_due": ("decisions", "propose_rows"),
    "_observed_state": ("decisions", "observed_row"),
    "_record_outcome": ("decisions", "record_outcome"),
    "_apply_effect": ("proposals", "apply_effect"),
}


def install_segments(
    controller: type,
    kernel_only: Mapping[str, tuple[str, str] | Callable[..., Any]] = KERNEL_ONLY_SEGMENTS,
) -> None:
    """Make each listed step of this class its kernel. Its bases keep the method's contract."""
    for step, kernel in kernel_only.items():
        if not callable(getattr(controller, step, None)):
            raise ValueError(f"the controller has no step {step}")
        label = step
        if not callable(kernel):
            label = f"{kernel[0]}.{kernel[1]}"
            kernel = getattr(native.rules(kernel[0]), kernel[1])
        setattr(controller, step, _only_kernel(controller, step, kernel, label))


def _only_kernel(
    controller: type, step: str, kernel: Callable[..., Any], label: str
) -> Callable[..., Any]:
    unsupported = native.Unsupported

    @wraps(getattr(controller, step))
    def run(self: Any, *arguments: Any, **options: Any) -> Any:
        try:
            return kernel(self, *arguments, **options)
        except unsupported as refusal:
            raise kernels.KernelRefusal(f"{label} refused: {refusal}") from refusal

    # `wraps` points this at the body-less method, which must not be called.
    del run.__wrapped__  # type: ignore[attr-defined]
    return run

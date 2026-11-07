"""Rust segments that replace a step of a phase, so a lane doesn't edit the phase's steps.

A phase's `_run_*_phase` lists its steps, each a method. A lane adds one line to SEGMENTS, and the
controller's step tries the kernel first. A kernel gets the controller and the step's arguments,
and returns what the step returns. Unsupported, raised before it has changed anything, runs the
Python step, so it says what the kernel doesn't handle: a tainted namespace, a policy it doesn't
know. See docs/rust-port/lanes/p4-world-core.md.

A step in KERNEL_ONLY_SEGMENTS has no Python step left (lane L10, docs/rust-port/step4-plan.md):
the phase's method only raises KernelRefusal, and the kernel's Unsupported becomes KernelRefusal,
as `kernel_only` of core/kernels.py does for a rule.
"""

from __future__ import annotations

from collections.abc import Callable, Mapping
from functools import wraps
from typing import Any

from sheltersim import native
from sheltersim.core import kernels

# Name of a step's method -> (the module of native/src/rules/, the name of its kernel).
SEGMENTS: dict[str, tuple[str, str]] = {
    "_view_after_movement": ("world_view", "view_after_movement"),
    "_view_for_proposals": ("world_view", "view_for_proposals"),
    "_propose_due": ("decisions", "propose_rows"),
    "_observed_state": ("decisions", "observed_row"),
    "_record_outcome": ("decisions", "record_outcome"),
    "_apply_effect": ("proposals", "apply_effect"),
    "_psychology_update": ("psychology", "update_psychology"),
    "_update_long_term_psychology": ("psychology", "update_long_term"),
    "_apply_state_deltas": ("psychology", "apply_deltas"),
    "_record_psychology_trace": ("trace", "record_psychology_trace"),
}

# Steps whose kernel alone runs them, in the same form.
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
}


def no_python_step(step: str) -> kernels.KernelRefusal:
    """What a phase's method of a KERNEL_ONLY_SEGMENTS step raises: it has no Python step.

    The controller's step is the kernel. The phase's method is reached only past it: through
    `super()`, which the tick's driver does after the kernel said Unsupported.
    """
    module, name = KERNEL_ONLY_SEGMENTS[step]
    return kernels.KernelRefusal(f"{module}.{name} refused, and {step} has no Python step")


def install_segments(
    controller: type,
    segments: Mapping[str, tuple[str, str] | Callable[..., Any]] = SEGMENTS,
    kernel_only: Mapping[str, tuple[str, str] | Callable[..., Any]] = KERNEL_ONLY_SEGMENTS,
) -> None:
    """Replace each listed step of this class. Its bases keep the Python step, or for a step of
    `kernel_only`, a method that has none."""
    for step, kernel in segments.items():
        if not callable(getattr(controller, step, None)):
            raise ValueError(f"the controller has no step {step}")
        if not callable(kernel):
            kernel = getattr(native.rules(kernel[0]), kernel[1])
        setattr(controller, step, _with_kernel(controller, step, kernel))
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

    return run


def _with_kernel(controller: type, step: str, kernel: Callable[..., Any]) -> Callable[..., Any]:
    python_step = getattr(controller, step)

    @wraps(python_step)
    def run(self: Any, *arguments: Any, **options: Any) -> Any:
        other = kernels._error_check
        seen = kernels._fell_back
        if other is None and seen is None:
            try:
                return kernel(self, *arguments, **options)
            except native.Unsupported:
                # Found on the bases at each call, so a test's patch of the Python step applies.
                return getattr(super(controller, self), step)(*arguments, **options)
        # The checks of core/kernels.py, on the Python step.
        rule = getattr(super(controller, self), step)
        mine = None if other is None else other.before(rule, arguments, options)
        try:
            return kernel(self, *arguments, **options)
        except native.Unsupported as refusal:
            if seen is None:
                return rule(*arguments, **options)
            try:
                return rule(*arguments, **options)
            except Exception as error:
                seen.fell_back(f"segment {step}", refusal, error)
                raise
        except Exception as error:
            if other is not None:
                other.after(rule, arguments, options, error, mine)
            raise

    return run

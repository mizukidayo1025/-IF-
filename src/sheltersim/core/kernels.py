"""A domain rule that is a Rust kernel, for rules that other modules call as plain functions.

A step of a phase's list goes in experiments/phases/segments.py. A function such as
staff_dynamics_on_shift is called from many modules and from tests, so it is its kernel where it is
defined, and every caller gets the kernel. See docs/rust-port/lanes/s-staff-dynamics.md.
"""

from __future__ import annotations

from collections.abc import Callable
from functools import wraps
from typing import Any, TypeVar

from sheltersim import native

Rule = TypeVar("Rule", bound=Callable[..., Any])


class KernelRefusal(ValueError):
    """A kernel said Unsupported: the input is outside the rule's contract.

    The rule has no Python body (lane L10, docs/rust-port/step4-plan.md): no Run reached a refusal
    of it, and only an input outside the rule's contract does. The text names the kernel and gives
    its reason: "<module>.<name> refused: <reason>". native/src/core/errors.rs (`kernel_refusal`)
    raises the same for a kernel the Rust code calls directly. A ValueError, so a caller that turns
    a rule's ValueError into an event's failure does so with this one too.
    """


def kernel_only(module: str, name: str) -> Callable[[Rule], Rule]:
    """native/src/rules/<module>.rs's kernel, under the decorated function's name and signature.

    The decorated function keeps the signature and the docstring and has no body. A refusal of the
    kernel raises KernelRefusal.
    """

    def decorate(entry: Rule) -> Rule:
        kernel = getattr(native.rules(module), name)
        unsupported = native.Unsupported
        label = f"{module}.{name}"

        @wraps(entry)
        def run(*arguments: Any, **options: Any) -> Any:
            try:
                return kernel(*arguments, **options)
            except unsupported as refusal:
                raise KernelRefusal(f"{label} refused: {refusal}") from refusal

        # `wraps` points this at the body-less function, which must not be called.
        del run.__wrapped__  # type: ignore[attr-defined]
        return run  # type: ignore[return-value]

    return decorate

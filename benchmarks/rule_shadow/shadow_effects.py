"""Compare the check of native/src/rules/effects.rs with its Python rule at each call.

`validate_state` only reads, and goes through shadow_kit.pure_shadow. When Python turns down what
it accepts, that is a difference: a check that takes a bad state is what a hash cannot see.

The guard, the two modifier checks and the handlers (facility, temporary space, structure,
environment, geometry) have no Python rule left to compare (`kernel_only`, lane L10).

Plain runs of the gate or of 7 days say what ran: put SHELTERSIM_RULE_SHADOW and
SHELTERSIM_DISPATCH_COUNT_ONLY=1 in the environment, and this only writes `effects.stats()`.
"""

from __future__ import annotations

import atexit
import os
from typing import Any

from shadow_kit import Recorder, pure_shadow

PURE = (("validate_state", "validate_effect_program_state"),)


def report_stats(recorder: Recorder, module: Any) -> None:
    def report() -> None:
        counts, reasons = module.stats()
        for key, count in counts.items():
            recorder.seen[f"ran.{key}"] += count
        for key, count in reasons.items():
            recorder.reasons[key] += count

    # After Recorder's own: atexit runs the last registered first.
    atexit.register(report)


def prepare(output: str) -> None:
    from sheltersim import native

    recorder = Recorder(output, "effects")
    rules = native.rules("effects")
    report_stats(recorder, rules)
    if os.environ.get("SHELTERSIM_DISPATCH_COUNT_ONLY") == "1":
        return
    unsupported = native.Unsupported
    module: list[Any] = []

    def effects() -> Any:
        if not module:
            from sheltersim.events import effects as loaded

            module.append(loaded)
        return module[0]

    for kernel_name, rule_name in PURE:
        setattr(
            rules,
            kernel_name,
            pure_shadow(
                recorder,
                kernel_name,
                getattr(rules, kernel_name),
                lambda rule_name=rule_name: getattr(effects(), rule_name).__wrapped__,
                unsupported,
            ),
        )

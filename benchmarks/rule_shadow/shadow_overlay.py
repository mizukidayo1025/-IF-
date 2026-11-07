"""Compare the kernels of rules/overlay.rs, rules/facility_queues.rs and the independent FIFO's.

These rules are rare and small beside a copy of the world, so a writer compares whole worlds: the
Python rule on one copy and the kernel on another, with their outcomes and `to_dict()` as JSON text.
The kernel's world must also verify its rows. The kernel then runs on the real world. A reader runs
the Python rule on the real world and compares results.

While a Python rule runs here every kernel of this shadow says Unsupported, so what is compared
is Python all the way down. A kernel a Python rule of another shadow reaches goes the same way on
that shadow's copy (`_Twin`).

A handler of events/effects.py calls these rules in Rust, past this file: to compare them too, run
shadow_effects.py with this one, whose Python rule comes here (docs/rust-port/lanes/m6-overlay.md).

SHELTERSIM_DISPATCH_COUNT_ONLY=1 only writes `overlay.stats()`, for a plain run.

The rules of domain/temporary_spaces.py, those of domain/facility_queues.py but
`queue_head_agent_ids`, `prepare_independent_facility_queue`, those of domain/geometry_overlay.py
but the check of the state, and the toilet's independent FIFO have no Python rule left to compare
(`kernel_only`).
"""

from __future__ import annotations

import copy
import importlib
import os
from typing import Any

from shadow_effects import report_stats
from shadow_kit import Recorder, text, values_in

OVERLAY_MODULE = "sheltersim.domain.geometry_overlay"
SPACES_MODULE = "sheltersim.domain.temporary_spaces"
QUEUES_MODULE = "sheltersim.domain.facility_queues"
QUEUES_KEY = "facility_queues_v1"
OVERLAY_KEY = "geometry_overlay_v1"

# (kernel, module of the rule, rule)
READERS = {
    "facility_queues": (("queue_head_agent_ids", QUEUES_MODULE, "queue_head_agent_ids"),),
}
# (rules module, kernel, module of the manager, its class, its method)
# The toilet's, the water and meal service's have no Python rule left to compare (`kernel_only`,
# lane L10).
SERVICE_WRITERS: tuple[tuple[str, str, str, str, str], ...] = ()
VALIDATORS = (
    ("overlay", "validate_spaces", SPACES_MODULE, "validate_temporary_spaces_state"),
    ("overlay", "validate_overlay", OVERLAY_MODULE, "validate_geometry_overlay_state"),
    ("facility_queues", "validate", QUEUES_MODULE, "validate_facility_queue_state"),
)


def _plain(value: Any) -> Any:
    """What a result says, for the text it is compared by."""
    if hasattr(value, "model_dump"):
        return value.model_dump(mode="json")
    if isinstance(value, (set, frozenset)):
        return sorted(value)
    if isinstance(value, tuple):
        return [_plain(item) for item in value]
    if isinstance(value, list):
        return [_plain(item) for item in value]
    if isinstance(value, dict):
        return {key: _plain(item) for key, item in value.items()}
    return value


def _rule(module: str, name: str) -> Any:
    return getattr(importlib.import_module(module), name)


def _unwrapped(rule: Any) -> Any:
    return getattr(rule, "__wrapped__", rule)


def _on_copy(first: Any) -> bool:
    """Whether a rule of another shadow runs on its stand-in for a world: the Python rules then."""
    from sheltersim.core.world import WorldState

    if isinstance(first, dict):
        return False
    return not isinstance(getattr(first, "world", first), WorldState)


def _verify(recorder: Recorder, world: Any, where: str) -> None:
    for name in (OVERLAY_KEY, QUEUES_KEY, "temporary_spaces_v1"):
        mirror = world.model_state._mirrors.get(name)
        if mirror is not None and name in world.model_state:
            recorder.seen["rows_verified"] += 1
            if not mirror.verify():
                recorder.differ(f"{where}: the row of {name} is not its tree")


def prepare(output: str) -> None:
    from sheltersim import native

    recorder = Recorder(output, "overlay")
    report_stats(recorder, native.rules("overlay"))
    if os.environ.get("SHELTERSIM_DISPATCH_COUNT_ONLY") == "1":
        return
    unsupported = native.Unsupported
    inside = [0]

    def python_rule(rule: Any, *arguments: Any, **options: Any) -> tuple[str, Any]:
        inside[0] += 1
        try:
            return "ok", _plain(rule(*arguments, **options))
        except Exception as raised:  # noqa: BLE001 - only compared with what the kernel does
            return type(raised).__name__, str(raised)
        finally:
            inside[0] -= 1

    def reader(label: str, kernel: Any, module: str, name: str) -> Any:
        def run(*arguments: Any, **options: Any) -> Any:
            if inside[0] or (arguments and _on_copy(arguments[0])):
                raise unsupported("a shadow's Python rule")
            recorder.seen[f"{label}.calls"] += 1
            expected = python_rule(_unwrapped(_rule(module, name)), *arguments, **options)
            try:
                result = kernel(*arguments, **options)
            except unsupported as reason:
                recorder.reasons[f"{label}: {reason}"] += 1
                if expected[0] == "ok":
                    recorder.seen[f"{label}.refused_a_success"] += 1
                raise
            recorder.seen[f"{label}.kernel_ran"] += 1
            if expected[0] != "ok":
                recorder.differ(f"{label}: the kernel took what Python turned down: {expected}")
            elif text(_plain(result)) != text(expected[1]):
                recorder.differ(f"{label}: {result!r:.200} is not {expected[1]!r:.200}")
            else:
                recorder.seen[f"{label}.values_compared"] += values_in(expected[1])
            return result

        return run

    def on_copies(
        label: str, kernel: Any, rule: Any, first: Any, build: Any, arguments: Any, options: Any
    ) -> None:
        """Both on their own copy of the world: the outcomes, the worlds, the kernel's rows."""
        from sheltersim.core.world import WorldState

        world = getattr(first, "world", first)
        where = f"{label} at {world.minute_index}"
        try:
            document = world.to_dict()
        except Exception:  # noqa: BLE001 - in the middle of another rule, nothing to copy
            recorder.seen[f"{label}.world_not_copyable"] += 1
            return
        python_world = WorldState.from_dict(document)
        kernel_world = WorldState.from_dict(document)
        expected = python_rule(rule, build(python_world), *arguments, **options)
        try:
            actual = ("ok", _plain(kernel(build(kernel_world), *arguments, **options)))
        except unsupported:
            raise
        except Exception as raised:  # noqa: BLE001 - only compared with the rule's
            actual = (type(raised).__name__, str(raised))
        recorder.seen[f"{label}.compared"] += 1
        if text(actual) != text(expected):
            recorder.differ(f"{where}: {actual!r:.300} is not {expected!r:.300}")
            return
        python_after = python_world.to_dict()
        kernel_after = kernel_world.to_dict()
        if text(python_after) != text(kernel_after):
            for key, value in python_after.items():
                if text(value) != text(kernel_after.get(key)):
                    recorder.differ(f"{where}: {key} differs")
        else:
            recorder.seen[f"{label}.values_compared"] += values_in(python_after)
        _verify(recorder, kernel_world, where)

    def service_writer(label: str, kernel: Any, module: str, class_name: str, method: str) -> Any:
        def run(manager: Any, *arguments: Any, **options: Any) -> Any:
            if inside[0] or _on_copy(manager):
                raise unsupported("a shadow's Python rule")
            recorder.seen[f"{label}.calls"] += 1
            manager_class = _rule(module, class_name)

            def on_the_copy(world: Any) -> Any:
                stand_in = copy.copy(manager)
                stand_in.world = world
                return stand_in

            try:
                on_copies(
                    label,
                    kernel,
                    _unwrapped(getattr(manager_class, method)),
                    manager,
                    on_the_copy,
                    arguments,
                    options,
                )
                result = kernel(manager, *arguments, **options)
            except unsupported as reason:
                recorder.reasons[f"{label}: {reason}"] += 1
                raise
            recorder.seen[f"{label}.kernel_ran"] += 1
            return result

        return run

    def validator(label: str, kernel: Any, module: str, name: str) -> Any:
        def run(world: Any) -> Any:
            if inside[0] or _on_copy(world):
                return False
            recorder.seen[f"{label}.calls"] += 1
            try:
                result = kernel(world)
            except unsupported as reason:
                recorder.reasons[f"{label}: {reason}"] += 1
                raise
            if result is True:
                recorder.seen[f"{label}.accepted"] += 1
                expected = python_rule(_rule(module, name), world)
                if expected[0] != "ok":
                    recorder.differ(f"{label}: took what Python turned down: {expected}")
            else:
                recorder.seen[f"{label}.declined"] += 1
            return result

        return run

    for module, entries in READERS.items():
        rules = native.rules(module)
        for kernel_name, rule_module, rule_name in entries:
            setattr(
                rules,
                kernel_name,
                reader(kernel_name, getattr(rules, kernel_name), rule_module, rule_name),
            )
    for rules_module, kernel_name, module, class_name, method in SERVICE_WRITERS:
        rules = native.rules(rules_module)
        setattr(
            rules,
            kernel_name,
            service_writer(
                f"{rules_module}.{kernel_name}",
                getattr(rules, kernel_name),
                module,
                class_name,
                method,
            ),
        )
    for module, kernel_name, rule_module, rule_name in VALIDATORS:
        rules = native.rules(module)
        setattr(
            rules,
            kernel_name,
            validator(
                f"{module}.{kernel_name}", getattr(rules, kernel_name), rule_module, rule_name
            ),
        )

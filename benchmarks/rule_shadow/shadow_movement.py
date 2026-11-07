"""Compare the validator of native/src/rules/movement.rs with its Python rule at each call.

The validator is compared on what it accepts: the Python rule must accept that too. What it refuses
goes to the Python rule anyway.

The other rules of domain/movement.py, `advance_movement` with them, have no Python rule left to
compare (`kernel_only`, lane L10).
"""

from __future__ import annotations

from typing import Any

from shadow_kit import Recorder

KEY = "movement_v1"


class _ReadOnlyQueue:
    def __init__(self, queue: Any) -> None:
        self.pending = queue.pending


class _Twin:
    """The world for a Python rule: a plain copy of the namespace and a queue it can't write.

    shadow_service.py's copies are of this kind."""

    def __init__(self, world: Any, plain: dict[str, Any]) -> None:
        table = dict(world.model_state)
        table[KEY] = plain
        self.__dict__.update(model_state=table, _world=world)
        self.__dict__["event_queue"] = _ReadOnlyQueue(world.event_queue)

    def __getattr__(self, name: str) -> Any:
        return getattr(self.__dict__["_world"], name)


def prepare(output: str) -> None:
    from sheltersim import native

    rules = native.rules("movement")
    recorder = Recorder(output, "movement")
    unsupported = native.Unsupported
    module: list[Any] = []

    def domain() -> Any:
        if not module:
            from sheltersim.domain import movement

            module.append(movement)
        return module[0]

    validate_kernel = rules.validate

    def validate(world: Any) -> bool:
        recorder.seen["validate.calls"] += 1
        try:
            accepted = validate_kernel(world)
        except unsupported as reason:
            recorder.reasons[f"validate: {reason}"] += 1
            raise
        recorder.seen[f"validate.{'accepted' if accepted else 'refused'}"] += 1
        if accepted:
            try:
                domain().validate_movement_runtime_state(world)
            except Exception as raised:  # noqa: BLE001 - only compared with what the kernel did
                recorder.differ(
                    f"validate: the kernel took what Python turned down: {raised!r:.300}"
                )
        return accepted

    rules.validate = validate

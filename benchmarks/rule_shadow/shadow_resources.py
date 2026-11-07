"""Compare the validator of native/src/state/resource_ledger.rs with its Python rule at each call.

Whatever the validator accepts must pass the Python rule. The operation, the KPI columns and the
used IDs of native/src/rules/resources.rs have no Python rule left to compare (`kernel_only`, lane
L10).
"""

from __future__ import annotations

from typing import Any

from shadow_kit import Recorder

KEY = "resource_ledger_v1"


def prepare(output: str) -> None:
    from sheltersim import native

    state = native.state("resource_ledger")
    recorder = Recorder(output, "resources")
    module: list[Any] = []

    def resources() -> Any:
        if not module:
            from sheltersim.domain import resources as loaded

            module.append(loaded)
        return module[0]

    validate_kernel = state.validate

    def validate(world: Any) -> bool:
        accepted = validate_kernel(world)
        recorder.seen[f"validate.{'accepted' if accepted else 'refused'}"] += 1
        if accepted:
            try:
                resources().validate_resource_ledger_state(
                    world.model_state[KEY],
                    world.resources,
                    set(world.agents),
                    set(world.zones),
                    minute_index=world.minute_index,
                )
            except ValueError as raised:
                recorder.differ(f"validate at {world.minute_index}: Python refused: {raised}")
            if recorder.seen["validate.accepted"] % 200 == 0:
                recorder.seen["validate.rows_verified"] += 1
                if not world._core.mirror(KEY).verify():
                    recorder.differ(f"validate at {world.minute_index}: the row and the tree")
        return accepted

    state.validate = validate

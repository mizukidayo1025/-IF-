"""Compare the validator of native/src/state/staff_dynamics.rs with its Python rule at each call.

The rules of native/src/rules/staff_dynamics.rs have no Python rule left to compare with (lane L10,
`kernel_only`), and the config's and an override's checks are Python's alone. What the validator
accepts, `validate_staff_dynamics_state` must accept too.
"""

from __future__ import annotations

from typing import Any

from shadow_kit import Recorder


def prepare(output: str) -> None:
    from sheltersim import native
    from sheltersim.core import state_mirror
    from sheltersim.domain import staff_dynamics

    recorder = Recorder(output, "staff_dynamics")
    rust_validate = native.state("staff_dynamics").validate

    def validator(world: Any) -> bool:
        accepted = rust_validate(world)
        recorder.seen["validator_accepted" if accepted else "validator_refused"] += 1
        try:
            staff_dynamics.validate_staff_dynamics_state(world)
        except ValueError as error:
            if accepted:
                recorder.differ(f"the validator took what Python turned down: {error}")
        else:
            if not accepted:
                recorder.seen["validator_refused_what_python_passes"] += 1
        return accepted

    state_mirror.register_rust_validator(staff_dynamics.STAFF_DYNAMICS_STATE_KEY, validator)

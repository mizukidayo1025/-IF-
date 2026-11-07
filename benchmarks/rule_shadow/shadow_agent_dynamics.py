"""Compare the validator of native/src/state/agent_dynamics.rs with its Python rule at each call.

The rules of native/src/rules/agent_dynamics.rs have no Python rule left to compare with (lane L10,
`kernel_only`). What the validator accepts, `validate_agent_dynamics_state` must accept too.
"""

from __future__ import annotations

from typing import Any

from shadow_kit import Recorder


def prepare(output: str) -> None:
    from sheltersim import native

    state_module = native.state("agent_dynamics")
    recorder = Recorder(output, "agent_dynamics")
    module: list[Any] = []

    def domain() -> Any:
        if not module:
            from sheltersim.domain import agent_dynamics

            module.append(agent_dynamics)
        return module[0]

    rust_validate = state_module.validate

    def validator(world: Any) -> bool:
        accepted = rust_validate(world)
        recorder.seen["validator_accepted" if accepted else "validator_refused"] += 1
        try:
            domain().validate_agent_dynamics_state(world)
        except ValueError as error:
            if accepted:
                recorder.differ(f"the validator took what Python turned down: {error}")
        else:
            if not accepted:
                recorder.seen["validator_refused_what_python_passes"] += 1
        return accepted

    state_module.validate = validator

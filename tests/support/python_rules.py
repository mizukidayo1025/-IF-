"""The `python_rules` mark: a test that validates with the Python validators, not their Rust twins.

The tick's rules are Rust kernels with no Python body (lane L10). The validators of `validate_*`
stay Python (decision D3 of docs/rust-port/step4-plan.md), and a Rust twin answers for a namespace
where it accepts it (`register_rust_validator` of core/state_mirror.py, and the entity rules of
core/world.py). `@pytest.mark.python_rules` takes those twins away for a test, so the Python
validators read every namespace: for a test that counts or patches what they read. Patches don't
reach a spawned worker of `ProjectAPI`, so neither does this mark.
"""

from __future__ import annotations

import pytest


@pytest.fixture(autouse=True)
def _python_rules(request: pytest.FixtureRequest, monkeypatch: pytest.MonkeyPatch) -> None:
    if request.node.get_closest_marker("python_rules") is None:
        return
    without_kernels(monkeypatch)


def without_kernels(monkeypatch: pytest.MonkeyPatch) -> None:
    """The mark's change, for a test that compares both within one test."""
    from sheltersim.core import state_mirror
    from sheltersim.core import world as world_module

    monkeypatch.setattr(state_mirror, "_RUST_VALIDATORS", {})
    monkeypatch.setattr(world_module, "_entity_rules_kernel", None)

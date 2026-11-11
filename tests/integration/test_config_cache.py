"""The kernels' configs are kept by the stamp of model_configs (Issue #152, lane 7-1).

native/src/core/configs.rs parses each section with the Python contracts at its first read after
the stamp changed, and keeps it in the core World (design-p7.md). A parse never raises there: a read
site replays what it raised, as a refusal, or, where the rule relays the error as it is, as the
error of a second parse, with the same class and text. An edit of model_configs in place is seen at
the next read; nothing makes one inside a tick phase (decision U1), and a debug run counts any.
"""

from __future__ import annotations

from typing import Any

import pytest
import sheltersim_native

from sheltersim import native
from sheltersim.contracts import policies
from sheltersim.core.state_mirror import field_stamp
from sheltersim.core.world import WorldState
from sheltersim.experiments.runner import run_scenario


def _information(world: WorldState) -> str:
    """An R read site: selected_models(...)["information"] in information_events.rs."""
    return native.rules("information_events").selected_information_of(world)


def _misinformation(world: WorldState) -> bool:
    """A U read site: _misinformation_applies in world_view.rs."""
    return native.rules("world_view").misinformation_applies_of(world)


@pytest.fixture
def parses(monkeypatch: pytest.MonkeyPatch) -> list[Any]:
    """The configs objects selected_models parsed, in order."""
    seen: list[Any] = []
    real = policies._select_models

    def counting(configs: Any) -> Any:
        seen.append(configs)
        return real(configs)

    monkeypatch.setattr(policies, "_select_models", counting)
    return seen


def test_the_selection_is_parsed_once_for_a_stamp(generated_scenario: Any, parses: list) -> None:
    world = WorldState.from_scenario(generated_scenario)
    expected = policies.selected_models(world.model_configs)
    del parses[:]
    stamp = field_stamp(world, "model_configs")
    assert _information(world) == expected["information"]
    assert _information(world) == expected["information"]
    assert _misinformation(world) is (
        expected["psychology"] in ("life-state-feedback-v1", "mean-reverting-life-state-v1")
    )
    assert parses == [world.model_configs]
    # Reading changes no stamp.
    assert field_stamp(world, "model_configs") == stamp


def test_a_write_in_place_is_seen_at_the_next_read(generated_scenario: Any, parses: list) -> None:
    world = WorldState.from_scenario(generated_scenario)
    _information(world)
    stamp = field_stamp(world, "model_configs")
    world.model_configs["behavior"]["temperature"] = -1.0
    assert field_stamp(world, "model_configs") != stamp
    with pytest.raises(ValueError) as python:
        policies.selected_models(world.model_configs)
    del parses[:]
    # R: the error as it is, from a second parse; U: the read site's refusal.
    with pytest.raises(ValueError) as relayed:
        _information(world)
    assert type(relayed.value) is type(python.value)
    assert (
        str(relayed.value)
        == str(python.value)
        == "behavior temperature must be finite and positive"
    )
    with pytest.raises(native.Unsupported, match="the Python rule raises"):
        _misinformation(world)
    # One parse kept the fault; the relayed error was parsed again.
    assert len(parses) == 2
    world.model_configs["behavior"]["temperature"] = 0.2
    assert _information(world) == policies.selected_models(world.model_configs)["information"]


def test_configs_no_mirror_follows_are_parsed_at_every_read(
    generated_scenario: Any, parses: list
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    world.model_configs = dict(world.model_configs)
    assert field_stamp(world, "model_configs") is None
    del parses[:]
    _information(world)
    _information(world)
    assert len(parses) == 2


def test_a_patched_selection_is_called_at_every_read(
    generated_scenario: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    kept = _information(world)
    real = policies.selected_models
    monkeypatch.setattr(policies, "selected_models", lambda c: {**real(c), "information": "x"})
    assert _information(world) == "x"
    monkeypatch.setattr(policies, "selected_models", lambda c: {**real(c), "information": "y"})
    assert _information(world) == "y"
    monkeypatch.undo()
    assert _information(world) == kept


def test_the_plan_of_the_end_of_tick_check_is_kept_with_the_configs(
    generated_scenario: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    from sheltersim.contracts import runtime

    plans: list[Any] = []
    real = runtime.declared_states

    def counting(configs: Any) -> Any:
        # Called once by tick_validation.tick_plan, and by other checks.
        plans.append(configs)
        return real(configs)

    monkeypatch.setattr(runtime, "declared_states", counting)
    world = WorldState.from_scenario(generated_scenario)
    del plans[:]
    world.validate_after_tick()
    assert world.__dict__["_tick_memo"].facts()["planned"]
    first = len(plans)
    del plans[:]
    # A new memo finds the plan of the same configs: tick_plan does not run again, and every
    # other reader of the declared states does.
    world.__dict__.pop("_tick_memo")
    world.validate_after_tick()
    assert world.__dict__["_tick_memo"].facts()["planned"]
    assert len(plans) == first - 1
    world.model_configs["behavior"]["temperature"] = -1.0
    for _ in range(2):
        # P: tick_plan's error out of validate_after_tick, every time.
        with pytest.raises(ValueError, match="behavior temperature must be finite and positive"):
            world.validate_after_tick()
        assert "_tick_memo" not in world.__dict__


def test_an_edit_in_place_inside_a_tick_phase_is_counted(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    sheltersim_native.watch_config_writes(True)
    try:
        world.model_configs["behavior"]["temperature"] = 0.3
        assert sheltersim_native.config_writes_in_tick() == 0
        with policies.selected_models_scope():
            world.model_configs["behavior"]["temperature"] = 0.2
        assert sheltersim_native.config_writes_in_tick() == 1
    finally:
        sheltersim_native.watch_config_writes(False)


def test_a_run_writes_no_configs_in_place_inside_a_tick(
    generated_scenario: Any, tmp_path: Any
) -> None:
    scenario = generated_scenario.model_copy(update={"duration_minutes": 120})
    sheltersim_native.watch_config_writes(True)
    try:
        run_scenario(scenario, tmp_path)
        assert sheltersim_native.config_writes_in_tick() == 0
    finally:
        sheltersim_native.watch_config_writes(False)

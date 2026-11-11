"""The configs' sections of lane 7-5b are kept by the stamp of model_configs (Issue #152).

The staff, the formulas, information, the body, agent dynamics, the zone environment,
infrastructure and proximity: each section is parsed at its first read after model_configs
changed and kept for that stamp (native/src/core/configs.rs). A read site raises what it raised
when it parsed at every read: a refusal (U) with its reason, an error it relays (R) as the Python
contract raises it, with the same class and text, from a second parse, and the text it embeds in
a failure (T). `sheltersim_native.config_section` reads a section through the read site's own
accessor.
"""

from __future__ import annotations

from typing import Any

import pytest
import sheltersim_native

from sheltersim import native
from sheltersim.core.state_mirror import field_stamp
from sheltersim.core.world import WorldState
from sheltersim.domain import environment, infrastructure, social_network
from sheltersim.domain.models import InformationSpreadConfig
from sheltersim.events.models import EventApplicationError
from sheltersim.policies import registry


def _read(world: WorldState, name: str, key: str | None = None) -> Any:
    return sheltersim_native.config_section(world, name, key)


@pytest.fixture
def world(generated_scenario: Any) -> WorldState:
    return WorldState.from_scenario(generated_scenario)


@pytest.fixture
def calls(monkeypatch: pytest.MonkeyPatch) -> dict[str, list[Any]]:
    """The calls of each Python parser a section runs, by name."""
    seen: dict[str, list[Any]] = {}

    def count(owner: Any, name: str, *, method: bool = False) -> None:
        real = getattr(owner, name)
        seen[name] = []
        if method:
            function = real.__func__

            def counted_method(cls: Any, *args: Any) -> Any:
                seen[name].append(args)
                return function(cls, *args)

            monkeypatch.setattr(owner, name, classmethod(counted_method))
        else:

            def counted(*args: Any) -> Any:
                seen[name].append(args)
                return real(*args)

            monkeypatch.setattr(owner, name, counted)

    count(registry, "create_staff_operations_formula")
    count(registry, "create_care_outcome_formula")
    count(social_network, "canonical_proximity_contact_config")
    count(InformationSpreadConfig, "from_mapping", method=True)
    count(environment.ScenarioZoneEnvironmentConfig, "model_validate", method=True)
    count(infrastructure.ScenarioInfrastructureSpec, "model_validate", method=True)
    return seen


# The sections a Python contract parses: the name of the read, of the parser, and an edit of the
# configs that changes what the read gives.
PYTHON_PARSED = [
    (
        "staff_operations",
        "create_staff_operations_formula",
        lambda c: c["policy_registry"].__setitem__("staff_operations", "staff-travel-first-v1"),
    ),
    (
        "care_outcome",
        "create_care_outcome_formula",
        lambda c: c["policy_registry"].__setitem__(
            "care_outcome", "hygiene-support-proportional-relief-v1"
        ),
    ),
    (
        "information_spread",
        "from_mapping",
        lambda c: c["information_spread"].__setitem__("query_pressure_per_claim", 0.25),
    ),
    (
        "information_pressure_gain",
        "from_mapping",
        lambda c: c["information_spread"].__setitem__("query_pressure_per_claim", 0.25),
    ),
    (
        "zone_environment",
        "model_validate",
        lambda c: c["zone_environment"].__setitem__("source_id", "ASM-TEST-ZONE"),
    ),
    (
        "proximity",
        "canonical_proximity_contact_config",
        lambda c: c.__setitem__(
            "social_proximity",
            {
                "schema_version": 1,
                "enabled": True,
                "policy_version": "distance-sampled-proximity-v1",
                "sample_interval_minutes": 5,
                "max_distance_m": 2,
                "max_pairs_per_sample": 10,
            },
        ),
    ),
]


@pytest.mark.parametrize(
    ("name", "parser", "edit"), PYTHON_PARSED, ids=[p[0] for p in PYTHON_PARSED]
)
def test_a_python_parsed_section_is_parsed_once_for_a_stamp(
    world: WorldState, calls: dict[str, list[Any]], name: str, parser: str, edit: Any
) -> None:
    first = _read(world, name)
    assert len(calls[parser]) == 1
    stamp = field_stamp(world, "model_configs")
    assert _read(world, name) == first
    assert len(calls[parser]) == 1
    # Reading changes no stamp; a write in place gives a new one, and the next read parses again.
    assert field_stamp(world, "model_configs") == stamp
    edit(world.model_configs)
    assert field_stamp(world, "model_configs") != stamp
    second = _read(world, name)
    assert second != first
    assert len(calls[parser]) == 2


def test_the_infrastructure_section_is_parsed_once_for_a_stamp(
    world: WorldState, calls: dict[str, list[Any]]
) -> None:
    world.model_configs["infrastructure"] = {"source_id": "ASM-TEST-INFRA"}
    calls["model_validate"].clear()
    assert _read(world, "infrastructure") == "ASM-TEST-INFRA"
    assert _read(world, "infrastructure") == "ASM-TEST-INFRA"
    assert len(calls["model_validate"]) == 1
    world.model_configs["infrastructure"]["source_id"] = "ASM-TEST-INFRA-2"
    assert _read(world, "infrastructure") == "ASM-TEST-INFRA-2"
    assert len(calls["model_validate"]) == 2


# The sections the kernels parse from the configs as a V, with an edit that changes them.
RUST_PARSED = [
    (
        "staff_fatigue",
        lambda c: c["policy_registry"].__setitem__("staff_fatigue", "staff-fatigue-saturating-v1"),
    ),
    ("support_task_minutes", lambda c: c["staff"].__setitem__("support_task_minutes", 25)),
    ("support_cooldown_minutes", lambda c: c["staff"].__setitem__("support_cooldown_minutes", 45)),
    (
        "social_interaction_minutes",
        lambda c: c["body_dynamics"].__setitem__("social_interaction_minutes", 9),
    ),
    (
        "body_models",
        lambda c: (
            c["policy_registry"].pop("suite_id"),
            c["policy_registry"].__setitem__("relationship", "duration-contact-trust-v1"),
        ),
    ),
    (
        "body_models",
        # Without a selected affect model, the config's own, through canonical_body_config.
        lambda c: (
            c["policy_registry"].pop("suite_id"),
            c["policy_registry"].pop("affect"),
            c["body_dynamics"].__setitem__("affect_policy", "trust-weighted-positive-negative-v1"),
        ),
    ),
    ("agent_dynamics_policy", lambda c: c.__setitem__("agent_dynamics", None)),
    (
        "facility_awareness",
        lambda c: c.__setitem__("facility_awareness", {"initial_known_facility_ids": []}),
    ),
    (
        "knowledge_is_limited",
        lambda c: [c.pop("agent_dynamics"), c.pop("facility_awareness", None)],
    ),
]


@pytest.mark.parametrize(("name", "edit"), RUST_PARSED, ids=[p[0] for p in RUST_PARSED])
def test_a_kernel_parsed_section_follows_a_write_in_place(
    world: WorldState, name: str, edit: Any
) -> None:
    first = _read(world, name)
    assert _read(world, name) == first
    edit(world.model_configs)
    assert _read(world, name) != first


def test_configs_no_mirror_follows_are_read_at_every_read(
    world: WorldState, calls: dict[str, list[Any]]
) -> None:
    world.model_configs = dict(world.model_configs)
    assert field_stamp(world, "model_configs") is None
    for _ in range(2):
        _read(world, "staff_operations")
        assert _read(world, "support_task_minutes") == 10
    assert len(calls["create_staff_operations_formula"]) == 2


def _same_error(read: Any, python: Any) -> BaseException:
    """What read raises, twice, is what python raises: the same class and text."""
    with pytest.raises(Exception) as expected:
        python()
    for _ in range(2):
        with pytest.raises(Exception) as relayed:
            read()
        assert type(relayed.value) is type(expected.value)
        assert str(relayed.value) == str(expected.value)
    return relayed.value


def test_a_relayed_error_is_the_contracts_own_at_every_read(
    world: WorldState, calls: dict[str, list[Any]]
) -> None:
    configs = world.model_configs
    # create_staff_operations_formula: an unknown formula is a KeyError of the factories.
    configs["policy_registry"]["staff_operations"] = "staff-unknown-v1"
    calls["create_staff_operations_formula"].clear()
    error = _same_error(
        lambda: _read(world, "staff_operations"),
        lambda: registry.create_staff_operations_formula(configs),
    )
    assert isinstance(error, KeyError)
    # One parse kept the fault, each read parsed again for the error, and the test once.
    assert len(calls["create_staff_operations_formula"]) == 1 + 2 + 1
    # canonical_proximity_contact_config.
    configs["social_proximity"] = {"enabled": True}
    _same_error(
        lambda: _read(world, "proximity"),
        lambda: social_network.canonical_proximity_contact_config(configs["social_proximity"]),
    )
    # ScenarioZoneEnvironmentConfig: pydantic's ValidationError, and the KeyError of no config.
    configs["zone_environment"]["noise_rest_efficiency_loss"] = 5.0
    error = _same_error(
        lambda: _read(world, "zone_environment"),
        lambda: environment.ScenarioZoneEnvironmentConfig.model_validate(
            configs["zone_environment"]
        ),
    )
    assert type(error).__name__ == "ValidationError"
    del configs["zone_environment"]
    _same_error(lambda: _read(world, "zone_environment"), lambda: configs["zone_environment"])
    # ScenarioInfrastructureSpec.
    configs["infrastructure"] = {"source_type": "survey"}
    _same_error(
        lambda: _read(world, "infrastructure"),
        lambda: infrastructure.ScenarioInfrastructureSpec.model_validate(configs["infrastructure"]),
    )
    # The cooldown's `.get` of a staff config that has none.
    configs["staff"] = ["not", "a", "dict"]
    _same_error(
        lambda: _read(world, "support_cooldown_minutes"),
        lambda: configs.get("staff", {}).get("support_cooldown_minutes", 30),
    )


def test_an_information_config_error_keeps_its_failure_text(world: WorldState) -> None:
    # T: the event fails with the text of the config's ValueError; the reader of the pressure
    # leaves it to the Python rule.
    world.model_configs["information_spread"]["enabled"] = "yes"
    with pytest.raises(ValueError) as python:
        InformationSpreadConfig.from_mapping(world.model_configs["information_spread"])
    for _ in range(2):
        with pytest.raises(EventApplicationError) as failed:
            _read(world, "information_spread")
        assert str(failed.value) == f"invalid information_spread config: {python.value}"
        with pytest.raises(native.Unsupported, match="the config is for the Python rule to read"):
            _read(world, "information_pressure_gain")
    # Without the key the event reads the class's own config, and the pressure reader refuses.
    del world.model_configs["information_spread"]
    with pytest.raises(EventApplicationError, match="information spread is not enabled"):
        _read(world, "information_spread")
    with pytest.raises(native.Unsupported, match="the config is for the Python rule to read"):
        _read(world, "information_pressure_gain")


@pytest.mark.parametrize(
    ("name", "edit", "reason"),
    [
        (
            "staff_fatigue",
            lambda c: c["policy_registry"].__setitem__("staff_fatigue", "staff-fatigue-other-v1"),
            "another fatigue formula",
        ),
        (
            "staff_operations_known",
            lambda c: c["policy_registry"].__setitem__("staff_operations", 3),
            "a value is not a str",
        ),
        (
            "support_task_minutes",
            lambda c: c["staff"].__setitem__("support_task_minutes", 2.5),
            "a value is not an int",
        ),
        (
            "support_cooldown_minutes",
            lambda c: c["staff"].__setitem__("support_cooldown_minutes", "30"),
            "a value is not an int",
        ),
        (
            "social_interaction_minutes",
            lambda c: c.__setitem__("body_dynamics", []),
            "a value is not a plain dict",
        ),
        (
            "body_models",
            lambda c: (
                c["policy_registry"].pop("suite_id"),
                c["policy_registry"].pop("affect"),
                c.__setitem__("body_dynamics", {"x": 1}),
            ),
            "the body config raises",
        ),
        (
            # The selection refuses a suite mixed with an override first.
            "care_outcome",
            lambda c: c["policy_registry"].__setitem__("relationship", "duration-contact-trust-v1"),
            "the model selection refuses",
        ),
        (
            "agent_dynamics_policy",
            lambda c: c.__setitem__("agent_dynamics", {"policy_version": "other"}),
            "a config the Python rule refuses",
        ),
        (
            "facility_awareness",
            lambda c: c.__setitem__("facility_awareness", {"schema_version": 2}),
            "another awareness schema",
        ),
    ],
)
def test_a_refusal_is_said_at_every_read(
    world: WorldState, name: str, edit: Any, reason: str
) -> None:
    edit(world.model_configs)
    for _ in range(2):
        with pytest.raises(native.Unsupported) as refused:
            _read(world, name)
        assert str(refused.value) == reason


def test_a_member_s_limit_is_refused_where_it_is_read(world: WorldState) -> None:
    world.model_configs["staff_dynamics"]["max_continuous_work_minutes_by_staff"] = {
        "s-ok": 90,
        "s-bad": "x",
    }
    assert _read(world, "staff_fatigue", "s-ok") == (False, 90)
    assert _read(world, "staff_fatigue", "s-none") == (False, None)
    with pytest.raises(native.Unsupported, match="a value is not an int"):
        _read(world, "staff_fatigue", "s-bad")


def test_a_fault_of_one_section_is_not_another_s(world: WorldState) -> None:
    world.model_configs["social_proximity"] = {"enabled": True}
    with pytest.raises(ValueError):
        _read(world, "proximity")
    assert _read(world, "support_task_minutes") == 10
    assert _read(world, "staff_operations")[2] == "staff-urgency-first-v1"
    world.model_configs["social_proximity"] = None
    assert _read(world, "proximity") is None


def test_sections_of_a_patched_selection_are_parsed_at_every_read(
    world: WorldState, calls: dict[str, list[Any]], monkeypatch: pytest.MonkeyPatch
) -> None:
    from sheltersim.contracts import policies

    kept = _read(world, "body_models")
    _read(world, "care_outcome")
    real = policies.selected_models
    monkeypatch.setattr(
        policies,
        "selected_models",
        lambda c: {**real(c), "relationship": "duration-contact-trust-v1"},
    )
    assert _read(world, "body_models") == (kept[0], "duration-contact-trust-v1")
    calls["create_care_outcome_formula"].clear()
    _read(world, "care_outcome")
    _read(world, "care_outcome")
    assert len(calls["create_care_outcome_formula"]) == 2
    monkeypatch.undo()
    assert _read(world, "body_models") == kept

"""social_propagation_v1 owned by Rust, and the social model's step in native/src/rules.

The step is rules/social_propagation.rs alone (docs/rust-port/lanes/b-social-propagation.md): the
Python rules of policies/social.py and policies/alternatives.py are gone. What they answered, on
small worlds and on values no scenario reaches, was recorded once as digests (ANSWERS), which the
kernel's answers must equal on every OS; a float's bits are compared through its hex.
"""

from __future__ import annotations

import hashlib
import json
import os
import random
from dataclasses import asdict, replace
from types import SimpleNamespace
from typing import Any

import pytest
from support.world import driven_ticks, view_of, watching_phases

from sheltersim import native
from sheltersim.core.engine import TickPhase, TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.experiments.runner import OfflineSimulationController, run_scenario
from sheltersim.policies.alternatives import DeGrootContactPolicy
from sheltersim.policies.behavior import WorldView
from sheltersim.policies.psychology import AgentStateDelta
from sheltersim.policies.social import (
    SOCIAL_RUNTIME_STATE_KEY,
    ExpressivenessChannelOpennessPolicy,
    SocialPropagationParameters,
    StressPropagationPolicy,
)

KEY = SOCIAL_RUNTIME_STATE_KEY
BOSSE = "ASM-BOSSE-AFFECT-CONTAGION-V1"
DEGROOT = "ASM-DEGROOT-CONTACT-V1"
POLICIES = {
    "sparse": (StressPropagationPolicy, "ASM-SOCIAL-PROPAGATION-V1"),
    "channel": (ExpressivenessChannelOpennessPolicy, BOSSE),
    "degroot": (DeGrootContactPolicy, DEGROOT),
}


def _rules() -> Any:
    return native.rules("social_propagation")


def _state() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "contacts": {
            '["a","b"]': {"last_seen_minute": 4, "consecutive_minutes": 2},
            '["b","a"]': {"last_seen_minute": 3, "consecutive_minutes": 1},
        },
        "agent_cooldown_until": {"b": 9, "a": 7},
    }


def _world(state: dict[str, Any] | None = None) -> WorldState:
    return WorldState("s", 1, 100, model_state={KEY: state if state is not None else _state()})


def _mirror(world: WorldState) -> Any:
    return world._core.mirror(KEY)


# ---- The row -----------------------------------------------------------------------------------


def test_a_row_writes_the_bytes_of_the_tree_with_its_keys_sorted() -> None:
    world = _world()
    assert _mirror(world).row_json() == native.canonical_json(_state())
    assert _mirror(world).verify()
    assert list(world.model_state[KEY]["agent_cooldown_until"]) == ["b", "a"]


@pytest.mark.parametrize(
    "write",
    [
        lambda state: state["contacts"]['["a","b"]'].__setitem__("last_seen_minute", 1.0),
        lambda state: state["contacts"]['["a","b"]'].__setitem__("consecutive_minutes", True),
        lambda state: state["contacts"]['["a","b"]'].__setitem__("other", 1),
        lambda state: state["contacts"]['["a","b"]'].pop("consecutive_minutes"),
        lambda state: state["contacts"].__setitem__("x", {"last_seen_minute": 1}),
        lambda state: state["agent_cooldown_until"].__setitem__("x", 1.5),
        lambda state: state.__setitem__("other", 1),
        lambda state: state.__setitem__("schema_version", "1"),
    ],
)
def test_a_value_the_row_cant_hold_taints_the_namespace(write: Any) -> None:
    world = _world()
    write(world.model_state[KEY])
    assert _mirror(world).tainted is not None
    assert not _mirror(world).has_row


def test_forwarded_writes_reach_the_row() -> None:
    world = _world()
    state = world.model_state[KEY]
    state["contacts"].pop('["a","b"]')
    state["agent_cooldown_until"].pop("a", None)
    state["contacts"]['["c","d"]'] = {"last_seen_minute": 5, "consecutive_minutes": 1}
    assert _mirror(world).verify()
    state["contacts"] = {k: v for k, v in state["contacts"].items() if "c" not in k}
    assert _mirror(world).verify()
    assert json.loads(_mirror(world).row_json())["contacts"].keys() == {'["b","a"]'}


# ---- The step on small worlds ------------------------------------------------------------------

# What the deleted Python rules answered (policies/social.py and policies/alternatives.py at
# 2d8fb46), as sha256 of the answers' texts, recorded once on Linux. With SHELTERSIM_RECORD_SOCIAL
# set, the tests wrote these digests of the Python rules' answers to that file instead.
ANSWERS: dict[str, str] = {
    "group": ("dd7c12c930b88477983e7ef0b47333e405e2dc251446845d128e4d1ab0d1ac5e"),
    "ints-channel-[('family_channel_strength', 1), ('same_zone_channel_strength', 0)]": (
        "f27599cb9e7be64bb0e04d3732512345eeb9379ef80ce5b9deea7ca3ec854164"
    ),
    "ints-channel-[('max_agent_stress_increase_per_minute', 0)]": (
        "78e08c31dd81258ece624b9625ff9f75c2056e444f6ed8caffb035d88e4b839c"
    ),
    "ints-channel-[('max_pair_stress_increase', 0), ('nearby_zone_channel_strength', 1)]": (
        "cbc200bf68ff866b3711a7e64733be0b206569a7d8ba11abc091c5ad32bb42bf"
    ),
    "ints-channel-[('max_pair_stress_increase', 0)]": (
        "d08ee46091c2329f707b75c73ad6d4e8e19df45ad9270a8a9e573063e76a0d9c"
    ),
    "ints-channel-[('natural_recovery_per_minute', 0), ('stress_spread_per_minute', 0)]": (
        "8b9d1886b023f0e87d5697ea2eba532852c554fb630fa81e45de6a9012f185c8"
    ),
    "ints-degroot-[('family_channel_strength', 1), ('same_zone_channel_strength', 0)]": (
        "071ace780e0a06f1c99488d2fee78bc95064ac8fe8566d67093e513e09ccd2fd"
    ),
    "ints-degroot-[('max_agent_stress_increase_per_minute', 0)]": (
        "6d2117d806b1b12e7b5213639112aa22b7d32c1e035dc79fc496804ffbf77556"
    ),
    "ints-degroot-[('max_pair_stress_increase', 0), ('nearby_zone_channel_strength', 1)]": (
        "d386c14a045db9f35648e564e32835f508bb923a993727bde0ed15b38d24e7e1"
    ),
    "ints-degroot-[('max_pair_stress_increase', 0)]": (
        "d386c14a045db9f35648e564e32835f508bb923a993727bde0ed15b38d24e7e1"
    ),
    "ints-degroot-[('natural_recovery_per_minute', 0), ('stress_spread_per_minute', 0)]": (
        "ebe9af54bc1d4aba7429d7ed14063790a212bab2e3f4c7ac57006b2676bb2bf8"
    ),
    "ints-sparse-[('family_channel_strength', 1), ('same_zone_channel_strength', 0)]": (
        "d7e36b1286cc9c9e344025fa620f5956ad7a92f19ade2aeb91c5e63e7c93e53c"
    ),
    "ints-sparse-[('max_agent_stress_increase_per_minute', 0)]": (
        "af75a4dfd95ebd5d7dbd9ddb8d07fa7b73e53c9d133dfac2098f490d0f2742c0"
    ),
    "ints-sparse-[('max_pair_stress_increase', 0), ('nearby_zone_channel_strength', 1)]": (
        "9000a526e3201281762b9f7e5a5e6391799f96194a775d0efbaa074b25812d96"
    ),
    "ints-sparse-[('max_pair_stress_increase', 0)]": (
        "9000a526e3201281762b9f7e5a5e6391799f96194a775d0efbaa074b25812d96"
    ),
    "ints-sparse-[('natural_recovery_per_minute', 0), ('stress_spread_per_minute', 0)]": (
        "c611db881b258e0203118183e718ae3f3980001bf1c3f85746256215366c74d3"
    ),
    "no-weight-channel_strength": (
        "ee34cc5baccaf04b28e8e07db7e40f6f44f0bb802d16be353aff841ca7afc76e"
    ),
    "no-weight-source_expressiveness": (
        "ee34cc5baccaf04b28e8e07db7e40f6f44f0bb802d16be353aff841ca7afc76e"
    ),
    "no-weight-stress_sensitivity": (
        "ee34cc5baccaf04b28e8e07db7e40f6f44f0bb802d16be353aff841ca7afc76e"
    ),
    "random-channel": ("b3c68b6b02dbe536a83670cb8b5ffa01d7ad8c91250ef92c48b6b3a034f6f037"),
    "random-degroot": ("f5bebc51bbbf2f949480df578091c55beea7c4bd1024b3f99e8cce020241235b"),
    "random-sparse": ("30443fb28baea6f03cec9351bdc890ed0e2648a9097c3816ffdd92f60c7e6681"),
    "restored": ("410610de6cf26039c13f61fba9fd4183998e4be8623ba1ed44bc27e26697156a"),
    "run-degroot-contact-v1": ("2519d1789c69d1d618b264953f27802b26d087cf533d7a29a7f47d4c67acde52"),
    "run-expressiveness-channel-openness-v1": (
        "d699ce16a6900720a790d2a35001feadff0f12ff091db070b6305ee9a343e439"
    ),
    "run-sparse-family-zone-contact-v1": (
        "23eb9a3c7f479b3085bf0ec4a723ea97b4aa84f739aab9a663db519a857a51f1"
    ),
    "step-channel": ("7f8ea0412eff688936fc0644a8f464773230ac675108c0eb0b37e7f8c349f289"),
    "step-degroot": ("70c26453be7b10a908dc0407375e76dada5998b7250b7acd259358f1092f483d"),
    "step-sparse": ("a805ab722e0a4f0b9b8223f82c66b388eb126180849b2b52045c59d1f5e39ea2"),
    "trace-channel-0": ("361ae546e0a0329277715bef8c63a62b45d1657562332487aafdef8b0ae5c4dd"),
    "trace-channel-1": ("1516096d50a09fad18da50faf43e730ced552e104c8fd51504e29844cdac22b0"),
    "trace-channel-2": ("5935d8fdcedd095fcca3261ec945889d15f6a5e8ec2a0252f92978f0c00a92c5"),
    "trace-channel-3": ("e43a1f504dcb29830f58cd04eb49a400e79cb57dc40e08890816a3bdef29cdc8"),
    "trace-degroot-0": ("8a8aac06e4021f51c912648f3e433091476021c9cf774225bf5a42e039c383c1"),
    "trace-degroot-1": ("76f74f88fe26ad98214844ba0b18c0764350d0bf10e21193b6b767d30538ca99"),
    "trace-degroot-2": ("3e7181475396149c842fe5e9556b28ec41b5558a79a3e7d6dab4e02b8b454808"),
    "trace-degroot-3": ("711fdf38ca20c7e6b85caa167e8033107d3bc178ffb1975ac8d4ef0baa167d34"),
    "trace-sparse-0": ("2d8bbe76417ddfcad2b6b7c3c11700ed23dfb0fefc6b53df7c1eba9a1e575f62"),
    "trace-sparse-1": ("7baf14348b0a758c011765886c7000e28b72d2c6461d390e3fa926eebb689d8c"),
    "trace-sparse-2": ("d54b14c13f866cfe0a37347b843760e7c4861a0a486cda53dff1f3e517badbc2"),
    "trace-sparse-3": ("5a87b37453a94349e6d4cfab0c996bd93b855aa78ce470bc6ae7a9b26887364b"),
}
_RECORD = os.environ.get("SHELTERSIM_RECORD_SOCIAL")


def _answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    if _RECORD:
        with open(_RECORD, "a", encoding="utf-8") as file:
            file.write(json.dumps({name: digest.hexdigest()}) + "\n")
        return
    assert digest.hexdigest() == ANSWERS[name], name


def _small(
    generated_scenario: Any, *, policy: str = "channel", **parameters: Any
) -> tuple[Any, ...]:
    """A world with a row, a controller, and the arguments of the step at the next minute."""
    world = WorldState.from_scenario(generated_scenario)
    world.model_state[KEY] = {"schema_version": 1, "contacts": {}, "agent_cooldown_until": {}}
    for index, agent in enumerate(world.agents.values()):
        agent.stress = (index % 10) / 10.0
    view = view_of(world, include_state_hash=False)
    kind, source = POLICIES[policy]
    settings = SocialPropagationParameters(
        enabled=True, stress_spread_per_minute=0.2, minimum_contact_minutes=1, source_id=source
    )
    controller = SimpleNamespace(
        stress_policy=kind(replace(settings, **parameters)),
        artifacts=SimpleNamespace(trace_level=TraceLevel.OFF),
        zone_neighbors={zone: () for zone in world.zones},
    )
    deltas = tuple(
        AgentStateDelta(
            agent_id=agent_id,
            minute_index=view.minute_index,
            toilet_urgency=0.0,
            hunger=0.0,
            fatigue=0.0,
            stress=0.5,
            trust=0.5,
        )
        for agent_id in sorted(view.agents)
    )
    return world, controller, view, deltas, CounterRNG(1)


def _python_result(world: Any, controller: Any, view: Any, deltas: Any, rng: Any) -> Any:
    """Only while recording: the deleted Python rule."""
    state = json.loads(json.dumps(world.model_state.get(KEY)))
    return controller.stress_policy.update(
        view, deltas, rng, zone_neighbors=controller.zone_neighbors, runtime_state=state
    )


def _run_kernel(world: Any, controller: Any, view: Any, deltas: Any, rng: Any) -> Any:
    return _rules().update_social_propagation(
        controller, world, view, deltas, view.minute_index, rng
    )


def _deltas_text(deltas: Any) -> str:
    return json.dumps([{**asdict(delta), "stress": delta.stress.hex()} for delta in deltas])


@pytest.mark.parametrize("policy", POLICIES)
def test_the_step_writes_what_the_python_rule_wrote(generated_scenario: Any, policy: str) -> None:
    arguments = _small(generated_scenario, policy=policy, cooldown_minutes=2)
    world = arguments[0]
    if _RECORD:
        python = _python_result(*arguments)
        _answers(f"step-{policy}", [json.dumps(python.runtime_state), _deltas_text(python.deltas)])
        return
    final = _run_kernel(*arguments)
    state = world.model_state[KEY]
    assert _mirror(world).row_json() == native.canonical_json(state)
    assert any(delta.stress != 0.5 for delta in final)
    _answers(f"step-{policy}", [json.dumps(state), _deltas_text(final)])


class _KeepingSink:
    """What a Run's sink is given, as text."""

    trace_level = TraceLevel.FULL

    def __init__(self) -> None:
        self.documents: list[str] = []

    def record_social_contact_trace(self, trace: Any) -> None:
        self.documents.append(json.dumps(trace, ensure_ascii=False))


@pytest.mark.parametrize("seed", range(4))
@pytest.mark.parametrize("policy", POLICIES)
def test_the_document_of_a_trace_is_the_python_rule_s(
    generated_scenario: Any, policy: str, seed: int
) -> None:
    """Over several minutes, so contacts last and cooldowns count, on values no scenario has."""
    draws = random.Random(seed)
    world, controller, _view, deltas, rng = _small(
        generated_scenario,
        policy=policy,
        stress_spread_per_minute=draws.choice((0.01, 0.2, 2.0)),
        max_pair_stress_increase=draws.choice((0.001, 0.05, 0.25)),
        max_agent_stress_increase_per_minute=draws.choice((0.002, 0.05, 0.25)),
        minimum_contact_minutes=draws.choice((1, 2)),
        cooldown_minutes=draws.choice((1, 2, 30)),
        natural_recovery_per_minute=draws.choice((0.0, 0.01)),
        max_contacts_per_agent=draws.choice((1, 3, 16)),
    )
    sink = _KeepingSink()
    controller.artifacts = sink
    texts = []
    for minute in range(5):
        world.minute_index = minute
        for agent in world.agents.values():
            agent.stress = draws.choice((0.0, 1.0, draws.random()))
        view = view_of(world, include_state_hash=False)
        base = tuple(
            replace(delta, minute_index=minute, stress=draws.choice((0.0, 1.0, draws.random())))
            for delta in deltas
        )
        if _RECORD:
            python = _python_result(world, controller, view, base, rng)
            texts += [
                json.dumps(python.trace_document(minute), ensure_ascii=False),
                json.dumps(python.runtime_state),
            ]
        _run_kernel(world, controller, view, base, rng)
        if not _RECORD:
            texts += [sink.documents[-1], json.dumps(world.model_state[KEY])]
    assert len(sink.documents) == 5
    _answers(f"trace-{policy}-{seed}", texts)


def test_records_change_keys_and_later_writes_reach_their_new_place(
    generated_scenario: Any,
) -> None:
    world, controller, view, deltas, rng = _small(generated_scenario)
    for minute in range(3):
        moved = replace(view, minute_index=minute)
        shifted = tuple(replace(delta, minute_index=minute) for delta in deltas)
        _run_kernel(world, controller, moved, shifted, rng)
        assert _mirror(world).verify()
    state = world.model_state[KEY]
    assert _mirror(world).stats()["whole_writes"] == 1
    # The root, two maps and the records, so nothing is left in the table.
    assert _mirror(world).stats()["live_nodes"] == 3 + len(state["contacts"])
    key = next(iter(state["contacts"]))
    state["contacts"][key]["consecutive_minutes"] = 99
    assert _mirror(world).verify()
    assert json.loads(_mirror(world).row_json())["contacts"][key]["consecutive_minutes"] == 99
    popped = state["contacts"].pop(key)
    popped["consecutive_minutes"] = 100
    assert key not in json.loads(_mirror(world).row_json())["contacts"]
    assert _mirror(world).verify()


def test_a_restored_namespace_gets_the_order_of_the_python_rule(generated_scenario: Any) -> None:
    """Keys sorted by the checkpoint's JSON, which a new dict of the rule does not have.

    The namespace is detached (test_detached_namespaces.py): a restore loads its row, and a read
    builds a tree in the row's order, which is the rule's."""
    world, controller, view, deltas, rng = _small(generated_scenario)
    _run_kernel(world, controller, view, deltas, rng)
    moved = replace(view, minute_index=1)
    shifted = tuple(replace(delta, minute_index=1) for delta in deltas)
    world.model_state[KEY] = json.loads(json.dumps(world.model_state[KEY], sort_keys=True))
    assert next(iter(next(iter(world.model_state[KEY]["contacts"].values())))) == (
        "last_seen_minute"
    )
    old_record = next(iter(world.model_state[KEY]["contacts"].values()))
    if _RECORD:
        _answers(
            "restored",
            [json.dumps(_python_result(world, controller, moved, shifted, rng).runtime_state)],
        )
        return
    _run_kernel(world, controller, moved, shifted, rng)
    state = world.model_state[KEY]
    # A new dict of the rule's: each record's keys in the order it writes them.
    assert all(
        list(record) == ["last_seen_minute", "consecutive_minutes"]
        for record in state["contacts"].values()
    )
    _answers("restored", [json.dumps(state)])
    assert _mirror(world).stats()["live_nodes"] == 3 + len(state["contacts"])
    old_record["consecutive_minutes"] = 5  # Out of the tree, so it reaches nothing.
    assert _mirror(world).verify()


def test_the_group_comes_from_the_view_and_not_the_profile(generated_scenario: Any) -> None:
    """A household in population_presence_v1 replaces profile.group_id in the view."""
    world, controller, view, deltas, rng = _small(generated_scenario)
    agents = {
        agent_id: replace(agent, group_id="everyone" if index % 2 else None)
        for index, (agent_id, agent) in enumerate(view.agents.items())
    }
    other = WorldView(minute_index=0, agents=agents, facilities={}, state_hash=None)
    if _RECORD:
        python = _python_result(world, controller, other, deltas, rng)
        _answers("group", [json.dumps(python.runtime_state)])
        return
    from_profiles = WorldState.from_scenario(generated_scenario)
    from_profiles.model_state[KEY] = json.loads(json.dumps(world.model_state[KEY]))
    for target, source in zip(from_profiles.agents.values(), world.agents.values(), strict=True):
        target.stress = source.stress
    _run_kernel(from_profiles, controller, view, deltas, rng)
    _run_kernel(world, controller, other, deltas, rng)
    assert world.model_state[KEY]["contacts"] != from_profiles.model_state[KEY]["contacts"]
    _answers("group", [json.dumps(world.model_state[KEY])])


def test_a_disabled_model_returns_the_deltas_and_writes_nothing(generated_scenario: Any) -> None:
    world, controller, view, deltas, rng = _small(generated_scenario, enabled=False)
    before = _mirror(world).row_json()
    assert _run_kernel(world, controller, view, deltas, rng) is deltas
    assert _mirror(world).row_json() == before


class _OtherRNG(CounterRNG):
    pass


class _OtherPolicy(ExpressivenessChannelOpennessPolicy):
    pass


@pytest.mark.parametrize(
    ("change", "reason"),
    [
        (lambda w, c, v, d, r: {"rng": _OtherRNG(1)}, "RNG"),
        (
            lambda w, c, v, d, r: {
                "controller": SimpleNamespace(
                    **{**vars(c), "stress_policy": _OtherPolicy(c.stress_policy.parameters)}
                )
            },
            "policy",
        ),
        (lambda w, c, v, d, r: {"deltas": d[:-1]}, "different people"),
        (lambda w, c, v, d, r: {"deltas": list(d)}, "tuple"),
        (
            lambda w, c, v, d, r: {"deltas": d[:-1] + (replace(d[-1], minute_index=7),)},
            "another minute",
        ),
        (lambda w, c, v, d, r: {"deltas": d[:-1] + (SimpleNamespace(**asdict(d[-1])),)}, "class"),
        (lambda w, c, v, d, r: {"view": replace(v, minute_index=7)}, "another minute"),
        (
            lambda w, c, v, d, r: {
                "controller": SimpleNamespace(
                    **{**vars(c), "zone_neighbors": {z: "ab" for z in c.zone_neighbors}}
                )
            },
            "tuple or a list",
        ),
        (
            lambda w, c, v, d, r: {
                "controller": SimpleNamespace(
                    **{**vars(c), "zone_neighbors": {z: (1,) for z in c.zone_neighbors}}
                )
            },
            "not a str",
        ),
    ],
)
def test_what_the_kernel_does_not_handle_is_unsupported_before_any_change(
    generated_scenario: Any, change: Any, reason: str
) -> None:
    """Inputs no tick gives the step: a test's RNG or policy class, deltas or a view of another
    tick, neighbors of another type. The step of a controller raises KernelRefusal for them."""
    world, controller, view, deltas, rng = _small(generated_scenario)
    arguments = {"controller": controller, "view": view, "deltas": deltas, "rng": rng}
    arguments |= change(world, controller, view, deltas, rng)
    before = _mirror(world).row_json()
    with pytest.raises(native.Unsupported, match=reason):
        _rules().update_social_propagation(
            arguments["controller"],
            world,
            arguments["view"],
            arguments["deltas"],
            view.minute_index,
            arguments["rng"],
        )
    assert _mirror(world).row_json() == before


@pytest.mark.parametrize(
    "corrupt",
    [
        lambda state: state.__setitem__("schema_version", 2),
        lambda state: state["contacts"].__setitem__(
            "x", {"last_seen_minute": -1, "consecutive_minutes": 1}
        ),
        lambda state: state["agent_cooldown_until"].__setitem__("x", -1),
        lambda state: state["agent_cooldown_until"].__setitem__("x", 1.5),
    ],
)
def test_a_state_the_rule_turns_down_is_a_refusal_of_the_step(
    generated_scenario: Any, corrupt: Any
) -> None:
    """A namespace no Run writes (the Python rule raised a ValueError on these): the step of a
    controller raises KernelRefusal and changes nothing."""
    world, controller, view, deltas, rng = _small(generated_scenario)
    corrupt(world.model_state[KEY])
    before = json.dumps(world.model_state[KEY])
    step = OfflineSimulationController._social_propagation
    controller.world = world
    with pytest.raises(KernelRefusal, match="update_social_propagation refused"):
        step(controller, world, view, deltas, view.minute_index, rng)
    assert json.dumps(world.model_state[KEY]) == before


def test_a_tainted_entity_store_is_a_refusal(generated_scenario: Any) -> None:
    world, controller, view, deltas, rng = _small(generated_scenario)
    next(iter(world.agents.values())).stress = 1
    with pytest.raises(native.Unsupported, match="tainted"):
        _run_kernel(world, controller, view, deltas, rng)


@pytest.mark.parametrize(
    "case", ["stress_sensitivity", "source_expressiveness", "channel_strength"]
)
def test_contacts_without_weight_shift_nothing(generated_scenario: Any, case: str) -> None:
    """#87 (元 #301): a total weight of 0 is no propagation.

    Before, the Python rule raised the ValueError of its strict zip and the kernel gave
    Unsupported, so a Run stopped. The trace, the row and the draws are the Python rule's.
    """
    document = generated_scenario.model_dump(mode="json")
    parameters: dict[str, Any] = {}
    if case == "stress_sensitivity":
        for profile in document["profiles"]:
            profile["stress_sensitivity"] = 0.0
    elif case == "source_expressiveness":
        parameters["source_expressiveness_by_agent"] = {
            profile["id"]: 0.0 for profile in document["profiles"]
        }
    else:
        parameters |= {
            f"{basis}_channel_strength": 0.0 for basis in ("family", "same_zone", "nearby_zone")
        }
    scenario = type(generated_scenario).model_validate(document)
    world, controller, view, deltas, _rng = _small(scenario, **parameters)
    sink = _KeepingSink()
    controller.artifacts = sink
    if _RECORD:
        python_rng = _KeepingRNG(1)
        python = _python_result(world, controller, view, deltas, python_rng)
        _answers(
            f"no-weight-{case}",
            [
                json.dumps(python_rng.draws),
                json.dumps(python.trace_document(0), ensure_ascii=False),
                json.dumps(python.runtime_state),
            ],
        )
        return
    _rules().start_draw_log()
    try:
        final = _run_kernel(world, controller, view, deltas, CounterRNG(1))
    finally:
        rust_draws = _rules().take_draw_log()
    # Contacts were made, and none of them moved a stress or started a cooldown.
    document_of = json.loads(sink.documents[0])
    assert document_of["selected_contact_count"] and final == deltas
    assert document_of["contributing_contact_count"] == 0
    assert world.model_state[KEY]["agent_cooldown_until"] == {}
    _answers(
        f"no-weight-{case}",
        [
            json.dumps([(key, value.hex()) for key, value in rust_draws]),
            sink.documents[0],
            json.dumps(world.model_state[KEY]),
        ],
    )


def test_unsorted_deltas_are_sorted_except_for_degroot(generated_scenario: Any) -> None:
    """The Python rules sorted the deltas by ID, except DeGroot's, which kept their order."""
    world, controller, view, deltas, rng = _small(generated_scenario)
    sorted_world = WorldState.from_scenario(generated_scenario)
    sorted_world.model_state[KEY] = json.loads(json.dumps(world.model_state[KEY]))
    for target, source in zip(sorted_world.agents.values(), world.agents.values(), strict=True):
        target.stress = source.stress
    backwards = tuple(reversed(deltas))
    final = _run_kernel(world, controller, view, backwards, rng)
    expected = _run_kernel(sorted_world, controller, view, deltas, rng)
    assert [asdict(delta) for delta in final] == [asdict(delta) for delta in expected]
    with pytest.raises(native.Unsupported, match="one person"):
        _run_kernel(world, controller, view, (deltas[0], *deltas[:-1]), rng)
    world, controller, view, deltas, rng = _small(generated_scenario, policy="degroot")
    with pytest.raises(native.Unsupported, match="id order"):
        _run_kernel(world, controller, view, tuple(reversed(deltas)), rng)


# ---- The step on a whole Run -------------------------------------------------------------------


@pytest.mark.parametrize(
    "policy",
    ["sparse-family-zone-contact-v1", "degroot-contact-v1", "expressiveness-channel-openness-v1"],
)
def test_every_tick_of_a_run_matches_the_python_rule_and_writes_no_whole_namespace(
    generated_scenario_with: Any, tmp_path: Any, policy: str
) -> None:
    """After each update phase, the namespace and the final deltas, which `_apply_state_deltas`
    wrote to the people and their body records. Their answers were recorded from the Python rule
    through the phases' Python steps, which went with #221; they are not recorded again."""
    if _RECORD:
        pytest.skip("the Python rule's answers of a Run were recorded through the phases")
    scenario = generated_scenario_with(social_policy_id=policy)
    document = scenario.model_dump(mode="json")
    document["duration_minutes"] = 90
    scenario = type(scenario).model_validate(document)
    texts: list[str] = []
    ran = []

    def counted(engine: Any, phase: TickPhase, before: bool) -> None:
        if phase is not TickPhase.UPDATE_AGENT_STATE or before:
            return
        world = engine.world
        assert engine.phase_handler.stress_policy.policy_version == policy
        ran.append(len(world.model_state[KEY]["contacts"]))
        texts.extend(
            [json.dumps(world.model_state[KEY]), _deltas_text(_written_deltas(world, len(ran) - 1))]
        )
        assert _mirror(world).verify()

    ticks = driven_ticks()
    with watching_phases(counted):
        world = run_scenario(scenario, tmp_path, trace_level=TraceLevel.OFF)
    assert driven_ticks() - ticks == 90
    assert len(ran) == 90 and sum(ran) > 0
    # The first tick makes the namespace, as the Python rule did.
    assert _mirror(world).stats()["whole_writes"] == 1
    _answers(f"run-{policy}", texts)


def _written_deltas(world: Any, minute: int) -> list[AgentStateDelta]:
    """The final deltas of the minute as `_apply_state_deltas` wrote them: each person's needs,
    stress and trust, and the body values of their record (None without one), in ID order."""
    records = world.model_state.get("body_state_v1", {}).get("agents", {})
    deltas = []
    for agent_id in sorted(world.agents):
        agent = world.agents[agent_id]
        record = records.get(agent_id)
        fields = {
            "agent_id": agent_id,
            "minute_index": minute,
            "toilet_urgency": agent.needs.toilet_urgency,
            "hunger": agent.needs.hunger,
            "fatigue": agent.needs.fatigue,
            "stress": agent.stress,
            "trust": agent.trust,
            "hydration_need": agent.needs.hydration_need,
        }
        for name in _BODY_VALUES:
            fields[name] = None if record is None else record[name]
        deltas.append(AgentStateDelta.within_bounds(fields))
    return deltas


_BODY_VALUES = (
    "sleep_debt_minutes",
    "uncertainty",
    "reassurance",
    "discontent",
    "positive_affect",
    "negative_affect",
    "hygiene_need",
)


def test_a_run_with_a_trace_is_the_same_world_as_one_without(
    generated_scenario: Any, tmp_path: Any
) -> None:
    document = generated_scenario.model_dump(mode="json")
    document["duration_minutes"] = 30
    scenario = type(generated_scenario).model_validate(document)
    traced = run_scenario(scenario, tmp_path / "traced", trace_level=TraceLevel.SUMMARY)
    plain = run_scenario(scenario, tmp_path / "plain", trace_level=TraceLevel.OFF)
    assert traced.canonical_json() == plain.canonical_json()
    # The kernel runs with a trace too, and rules/trace.rs writes its document.
    assert _mirror(traced).stats()["whole_writes"] == 1
    assert _mirror(plain).stats()["whole_writes"] == 1


@pytest.mark.parametrize("policy", POLICIES)
@pytest.mark.parametrize(
    "ints",
    [
        {"max_pair_stress_increase": 0},
        {"max_agent_stress_increase_per_minute": 0},
        {"stress_spread_per_minute": 0, "natural_recovery_per_minute": 0},
        {"family_channel_strength": 1, "same_zone_channel_strength": 0},
        {"nearby_zone_channel_strength": 1, "max_pair_stress_increase": 0},
    ],
    ids=str,
)
def test_int_coefficients_of_a_scenario_give_the_python_rule_s_answers(
    generated_scenario: Any, policy: str, ints: dict[str, int]
) -> None:
    """A Scenario may write a coefficient as `0` or `1`. The kernel turned these down before and
    the Python rule ran; now it takes them as the Python rule did, keeping an int cap an int in
    the trace (`min(0, x)` is the int 0, which JSON writes as `0`)."""
    world, controller, _view, deltas, rng = _small(
        generated_scenario, policy=policy, cooldown_minutes=2, **ints
    )
    neighbors = sorted(world.zones)
    controller.zone_neighbors = {zone: tuple(neighbors) for zone in neighbors}
    if policy == "channel":
        first = sorted(world.agents)[0]
        controller.stress_policy = POLICIES[policy][0](
            replace(controller.stress_policy.parameters, source_expressiveness_by_agent={first: 0})
        )
    sink = _KeepingSink()
    controller.artifacts = sink
    texts = []
    draws = random.Random(7)
    for minute in range(4):
        world.minute_index = minute
        for agent in world.agents.values():
            agent.stress = draws.choice((0.0, 1.0, draws.random()))
        view = view_of(world, include_state_hash=False)
        base = tuple(
            replace(delta, minute_index=minute, stress=draws.choice((0.0, 1.0, draws.random())))
            for delta in deltas
        )
        if _RECORD:
            python = _python_result(world, controller, view, base, rng)
            texts += [
                json.dumps(python.trace_document(minute), ensure_ascii=False),
                json.dumps(python.runtime_state),
                _deltas_text(python.deltas),
            ]
        final = _run_kernel(world, controller, view, base, rng)
        if not _RECORD:
            texts += [sink.documents[-1], json.dumps(world.model_state[KEY]), _deltas_text(final)]
    _answers(f"ints-{policy}-{sorted(ints.items())}", texts)


# ---- The bits, on values no scenario reaches -----------------------------------------------------


class _KeepingRNG(CounterRNG):
    def __init__(self, seed: int) -> None:
        super().__init__(seed)
        self.draws: list[tuple[str, str]] = []

    def random(self, *key_parts: Any) -> float:
        value = super().random(*key_parts)
        key = json.dumps(list(key_parts), ensure_ascii=False, separators=(",", ":"))
        self.draws.append((key, value.hex()))
        return value


def _plan(
    policy: str, seed: int, view: Any, deltas: Any, settings: Any, neighbors: Any, state: Any
) -> Any:
    ids = sorted(view.agents)
    base = {delta.agent_id: delta.stress for delta in deltas}
    return _rules().plan_of_values(
        policy,
        str(seed),
        view.minute_index,
        [
            settings.max_contacts_per_agent,
            settings.minimum_contact_minutes,
            settings.cooldown_minutes,
        ],
        [
            settings.stress_spread_per_minute,
            settings.natural_recovery_per_minute,
            settings.max_pair_stress_increase,
            settings.max_agent_stress_increase_per_minute,
            settings.family_channel_strength,
            settings.same_zone_channel_strength,
            settings.nearby_zone_channel_strength,
        ],
        ids,
        [
            (
                view.agents[agent_id].group_id,
                view.agents[agent_id].zone_id,
                view.agents[agent_id].stress,
                view.agents[agent_id].stress_sensitivity,
                settings.source_expressiveness_by_agent.get(agent_id, 1.0),
                base[agent_id],
            )
            for agent_id in ids
        ],
        {zone: list(names) for zone, names in neighbors.items()},
        state["contacts"],
        state["agent_cooldown_until"],
    )


def _random_case(draw: random.Random, template: Any, policy: str) -> tuple[Any, ...]:
    names = ["A1", "A2", 'A"3', "A\\4", "é5", "ß6", "A\n7", "B8", "B9", "C10", "C11", "日12"]
    zones = ["z1", "z2", "zé", 'z"4']
    stresses = [0.0, 1.0, 0.5, 0.25, 1e-300, 1.0 - 2**-53]

    def unit() -> float:
        return draw.choice(stresses) if draw.random() < 0.3 else draw.random()

    minute = draw.randrange(0, 50)
    ids = draw.sample(names, draw.randrange(1, len(names) + 1))
    agents = {
        agent_id: replace(
            template,
            id=agent_id,
            zone_id=draw.choice(zones),
            group_id=draw.choice((None, "g1", "g2", 'g"3')),
            stress=unit(),
            stress_sensitivity=draw.choice((0.0, 1.0, draw.random())),
        )
        for agent_id in ids
    }
    view = WorldView(minute_index=minute, agents=agents, facilities={}, state_hash=None)
    neighbors = {
        zone: tuple(draw.sample(zones, draw.randrange(0, len(zones) + 1)))
        for zone in zones
        if draw.random() < 0.8
    }
    contacts = {
        json.dumps([a, b], ensure_ascii=False, separators=(",", ":")): {
            "last_seen_minute": draw.choice((minute - 1 if minute else 0, minute, 0)),
            "consecutive_minutes": draw.randrange(1, 12),
        }
        for a in names
        for b in names
        if a != b and draw.random() < 0.3
    }
    state = {
        "schema_version": 1,
        "contacts": contacts,
        "agent_cooldown_until": {
            name: draw.randrange(0, 60) for name in names if draw.random() < 0.4
        },
    }
    deltas = tuple(
        AgentStateDelta(
            agent_id=agent_id,
            minute_index=minute,
            toilet_urgency=0.0,
            hunger=0.0,
            fatigue=0.0,
            stress=unit(),
            trust=0.5,
        )
        for agent_id in sorted(ids)
    )
    settings = SocialPropagationParameters(
        enabled=True,
        max_contacts_per_agent=draw.randrange(0, 5),
        minimum_contact_minutes=draw.randrange(1, 6),
        stress_spread_per_minute=draw.choice((0.0, 0.25, draw.random() / 4)),
        natural_recovery_per_minute=draw.choice((0.0, 0.25, draw.random() / 4)),
        max_pair_stress_increase=draw.choice((0.0, 0.05, 1e-6, draw.random() / 4)),
        max_agent_stress_increase_per_minute=draw.choice((0.0, 0.05, draw.random() / 4)),
        cooldown_minutes=draw.randrange(1, 40),
        source_expressiveness_by_agent={
            name: draw.choice((0.0, 1.0, draw.random())) for name in names if draw.random() < 0.5
        },
        family_channel_strength=draw.choice((0.0, 1.0, draw.random())),
        same_zone_channel_strength=draw.choice((0.0, 1.0, draw.random())),
        nearby_zone_channel_strength=draw.choice((0.0, 1.0, draw.random())),
        source_id=POLICIES[policy][1],
    )
    return view, deltas, settings, neighbors, state


def _template(generated_scenario: Any) -> Any:
    world = WorldState.from_scenario(generated_scenario)
    return next(iter(view_of(world, include_state_hash=False).agents.values()))


@pytest.mark.parametrize("policy", POLICIES)
def test_the_rule_gives_the_python_rule_s_answers_on_random_inputs(
    generated_scenario: Any, policy: str
) -> None:
    """IDs that JSON escapes, a zone next to itself, empty channels, zero sensitivity, exact caps,
    and every cooldown and contact history a Run may not reach by itself. Each draw too."""
    draw = random.Random(2615)
    template = _template(generated_scenario)
    texts: list[str] = []
    stress_moved = draws = 0
    for _trial in range(400):
        view, deltas, settings, neighbors, state = _random_case(draw, template, policy)
        seed = draw.randrange(0, 2**70)
        ids = sorted(view.agents)
        if _RECORD:
            rng = _KeepingRNG(seed)
            python = POLICIES[policy][0](settings).update(
                view, deltas, rng, zone_neighbors=neighbors, runtime_state=state
            )
            by_id = {delta.agent_id: delta.stress for delta in python.deltas}
            answer = [
                rng.draws,
                [by_id[agent_id].hex() for agent_id in ids],
                [
                    [key, record["last_seen_minute"], record["consecutive_minutes"]]
                    for key, record in python.runtime_state["contacts"].items()
                ],
                sorted(python.runtime_state["agent_cooldown_until"].items()),
                [[item.recipient_id, item.source_id] for item in python.contacts],
            ]
            texts.append(json.dumps(answer, ensure_ascii=False))
            continue
        _rules().start_draw_log()
        try:
            stresses, contacts, cooldowns, pairs = _plan(
                policy, seed, view, deltas, settings, neighbors, state
            )
        finally:
            rust_draws = _rules().take_draw_log()
        draws += len(rust_draws)
        answer = [
            [[key, value.hex()] for key, value in rust_draws],
            [value.hex() for value in stresses],
            [[key, last, count] for key, last, count in contacts],
            sorted(dict(cooldowns).items()),
            [[ids[a], ids[b]] for a, b in pairs],
        ]
        texts.append(json.dumps(answer, ensure_ascii=False))
        by_id = dict(zip(ids, stresses, strict=True))
        stress_moved += any(delta.stress != by_id[delta.agent_id] for delta in deltas)
    if not _RECORD:
        assert stress_moved > 5 and draws > 1000
    _answers(f"random-{policy}", texts)


@pytest.mark.parametrize("policy", POLICIES)
def test_the_comparison_sees_a_change_of_one_percent(generated_scenario: Any, policy: str) -> None:
    draw = random.Random(2615)
    template = _template(generated_scenario)
    seen = 0
    for _ in range(400):
        view, deltas, settings, neighbors, state = _random_case(draw, template, policy)
        try:
            right = _plan(policy, 1, view, deltas, settings, neighbors, state)
        except native.Unsupported:
            continue
        spread = settings.stress_spread_per_minute * 0.99
        off = replace(settings, stress_spread_per_minute=spread)
        wrong = _plan(policy, 1, view, deltas, off, neighbors, state)
        seen += right[0] != wrong[0]
    assert seen > 0

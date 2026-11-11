"""deepcopy(world) forks the core, and a preview's copy is a fork in canonical order (#152, 6-6).

``copy.deepcopy(world)`` makes a new core that shares every part of the state with the world's
until either writes it, and new entity objects and trees from the rows
(``state_mirror.deepcopy_world``, design-p5-p6.md §7). It must hold what the old copy held (every
attribute copied as plain containers and owned again by a new core): the same canonical JSON, and
every map in the same order with the same types. The two worlds must stay apart: a write to one,
of any kind, never shows in the other. ``state_mirror.canonical_copy`` gives the previews (the
added toilets' floor-plan check and a model-state proposal) the copy through canonical JSON they
had, with the same checks, and leaves the world in its order.
"""

from __future__ import annotations

import copy
from typing import Any

import pytest
from support.paths import SMALL_REPLAY
from support.world_order import (
    Run,
    check_rows,
    fingerprint,
    first_difference,
    gate_runs,
    identities,
    write_everything,
)

from sheltersim import native
from sheltersim.contracts import runtime as contracts
from sheltersim.contracts.workspace import StateDescriptor
from sheltersim.core import state_mirror
from sheltersim.core.engine import TickEngine
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.simulator_state import initialize_simulator_extensions
from sheltersim.events import effects
from sheltersim.events.effects import validate_effect_program
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink


def _old_copy(world: WorldState, monkeypatch: pytest.MonkeyPatch) -> WorldState:
    """deepcopy(world) as it was before the fork: every attribute copied and owned again."""
    with monkeypatch.context() as patch:
        patch.setattr(state_mirror, "_fork_core", lambda *_: None)
        return copy.deepcopy(world)


def _forks(world: WorldState) -> bool:
    core = world.__dict__["_core"]
    return state_mirror._fork_core(world, core, {}) is not None


def _same(mine: WorldState, theirs: WorldState, label: Any) -> None:
    assert mine.canonical_json() == theirs.canonical_json(), label
    first, second = fingerprint(mine), fingerprint(theirs)
    assert first == second, (label, first_difference(first, second))


def _odd_parts(world: WorldState) -> None:
    """What no row holds, or holds otherwise than between ticks: a namespace that stays detached
    but has its tree now, a key whose value V can't hold, a key no lane owns, and metric_state
    assigned whole past its tree."""
    world.model_state["movement_v1"]  # a read builds the tree again
    world.model_state["custom_pair_v1"] = {"pair": (1, 2), "b": 1}
    world.model_state["custom_value_v1"] = {"z": [1, {"y": 2}], "a": None}
    world.metric_state = {"assigned_whole": 1.0}


# ---- deepcopy holds what the old copy held -------------------------------------------------------


def test_deepcopy_holds_what_the_old_copy_held_in_the_same_order(
    add_toilets_program: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    for label, run in gate_runs(add_toilets_program).items():
        for minute in (0, 25, 70, 160):
            world = run.to(minute)
            assert _forks(world), (label, minute)
            # Its check reads the namespaces it validates in Python, which builds their trees.
            text = world.canonical_json()
            before = fingerprint(world)
            forked = copy.deepcopy(world)
            old = _old_copy(world, monkeypatch)
            _same(forked, old, (label, minute))
            check_rows(forked)
            assert forked.__dict__["_core"] is not world.__dict__["_core"]
            assert list(forked.__dict__) == list(old.__dict__)
            # The world is read, never written or reordered.
            assert fingerprint(world) == before
            assert world.canonical_json() == text


def test_deepcopy_copies_what_no_row_holds_as_before(
    add_toilets_program: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(40)
    _odd_parts(world)
    assert world.__dict__["_core"].mirror("custom_pair_v1").tainted is not None
    assert _forks(world)
    forked = copy.deepcopy(world)
    _same(forked, _old_copy(world, monkeypatch), "odd parts")
    table = forked.__dict__["model_state"]
    assert dict.__getitem__(table, "movement_v1") is state_mirror._DETACHED
    assert forked.model_state["custom_pair_v1"]["pair"] == (1, 2)
    assert forked.__dict__["_core"].mirror("custom_pair_v1").tainted is not None
    check_rows(forked)


def test_a_world_its_rows_do_not_all_hold_is_copied_as_before(
    add_toilets_program: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(30)
    first = next(iter(world.agents))
    # An object put past IdMap under its key: the rows no longer say what the map holds.
    dict.__setitem__(world.agents, first, copy.copy(world.agents[first]))
    assert not _forks(world)
    _same(copy.deepcopy(world), _old_copy(world, monkeypatch), "past IdMap")
    world = gate_runs(add_toilets_program)["generated-operations"].to(30)
    with state_mirror.transaction(world, "full"):
        assert not _forks(world)
        _same(copy.deepcopy(world), _old_copy(world, monkeypatch), "in a transaction")
    world.__dict__["_core"].entities().taint_now("a test taints the store")
    assert not _forks(world)
    _same(copy.deepcopy(world), _old_copy(world, monkeypatch), "tainted")


def test_deepcopy_keeps_what_the_memo_shares(add_toilets_program: Any) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(10)
    agent = next(iter(world.agents.values()))
    forked, forked_agent, forked_table = copy.deepcopy((world, agent, world.model_state))
    assert forked_agent is next(iter(forked.agents.values()))
    assert forked_table is forked.__dict__["model_state"]
    # A memo that holds a part already: the old copy reads it from there.
    memo = {id(world.agents): world.agents}
    assert state_mirror._fork_core(world, world.__dict__["_core"], memo) is None


# ---- The two worlds stay apart -------------------------------------------------------------------


def test_a_write_to_a_fork_or_its_world_never_shows_in_the_other(
    add_toilets_program: Any,
) -> None:
    run = gate_runs(add_toilets_program)["generated-operations"]
    world = run.to(70)
    world.event_queue.pending()  # every pending event has its object
    text = world.canonical_json()
    before = fingerprint(world)
    held = identities(world)
    forked = copy.deepcopy(world)
    copied = fingerprint(forked)
    write_everything(forked)
    assert fingerprint(world) == before
    assert world.canonical_json() == text
    assert identities(world) == held
    check_rows(world)
    check_rows(forked)
    # And the other way: the world runs on, the fork made before stays as it was.
    again = copy.deepcopy(world)
    run.to(90)
    write_everything(world)
    mine = fingerprint(again)
    assert mine == copied, first_difference(mine, copied)
    assert again.canonical_json() == text
    check_rows(again)
    check_rows(world)


def test_a_fork_runs_on_as_its_world_does(add_toilets_program: Any) -> None:
    run = gate_runs(add_toilets_program)["generated-operations"]
    world = run.to(50)
    forked = copy.deepcopy(world)
    other = Run.__new__(Run)
    other.world = forked
    other.engine = TickEngine(
        forked, phase_handler=OfflineSimulationController(forked, _MetricSink(MetricsRecorder()))
    )
    run.to(120)
    other.to(120)
    assert forked.canonical_json() == world.canonical_json()
    check_rows(forked)


# ---- The previews' copy --------------------------------------------------------------------------


@pytest.mark.parametrize("kind", ["full", "effect"])
def test_a_preview_copy_holds_what_the_copy_through_canonical_json_held(
    add_toilets_program: Any, kind: str
) -> None:
    for label, run in gate_runs(add_toilets_program).items():
        for minute in (0, 25, 70, 160):
            world = run.to(minute)
            world.validate()  # builds the trees its Python checks read
            before = fingerprint(world)
            candidate = state_mirror.canonical_copy(world, kind)
            assert not candidate.__dict__["_core"].in_transaction
            _same(candidate, state_mirror.copy_through_json(world, kind), (label, minute, kind))
            check_rows(candidate)
            assert (candidate.event_log is world.event_log) == (kind == "effect")
            assert "_tick_memo" not in candidate.__dict__
            # The world keeps its order.
            assert fingerprint(world) == before


def test_a_preview_copy_is_checked_as_before(
    add_toilets_program: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(30)
    original = WorldState.validate
    for failing_call, expected, text in (
        (1, ValueError, "^the world is wrong$"),
        (2, StateInvariantError, "^invalid WorldState document: 'the copy is wrong'$"),
    ):
        for kind in ("full", "effect"):
            raised = []
            for make in (state_mirror.canonical_copy, state_mirror.copy_through_json):
                calls: list[int] = []

                def failing(self: WorldState, *arguments: Any, **keywords: Any) -> Any:
                    calls.append(len(calls))
                    if len(calls) == failing_call:
                        raise (ValueError if failing_call == 1 else KeyError)(
                            "the world is wrong" if failing_call == 1 else "the copy is wrong"
                        )
                    return original(self, *arguments, **keywords)

                monkeypatch.setattr(WorldState, "validate", failing)
                with pytest.raises(expected, match=text) as error:
                    make(world, kind)
                monkeypatch.setattr(WorldState, "validate", original)
                raised.append((type(error.value), str(error.value)))
            assert raised[0] == raised[1], (kind, raised)


def test_a_world_a_transaction_refuses_is_copied_through_canonical_json(
    add_toilets_program: Any,
) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(20)
    dict.__setitem__(world.model_state, 7, {"a": 1})
    with pytest.raises(native.Unsupported):
        with state_mirror.transaction(copy.deepcopy(world), "full"):
            pass
    for kind in ("full", "effect"):
        raised = []
        for make in (state_mirror.canonical_copy, state_mirror.copy_through_json):
            # canonical JSON can't sort a key that is not a str among str keys.
            with pytest.raises(TypeError) as error:
                make(world, kind)
            raised.append(str(error.value))
        assert raised[0] == raised[1], kind


def _world_with_extensions(scenario: ScenarioSpec) -> WorldState:
    world = WorldState.from_scenario(scenario)
    initialize_simulator_extensions(world)
    return world


def test_the_toilet_floor_plan_check_finds_what_it_found(
    generated_scenario: ScenarioSpec, add_toilets_program: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world = _world_with_extensions(generated_scenario)
    world.validate()  # builds the trees its Python checks read
    found = {}
    for count in (4, 5):
        program = add_toilets_program(count, start_minute=0)
        before = fingerprint(world)
        found[count] = validate_effect_program(world, program)
        assert fingerprint(world) == before
        with monkeypatch.context() as patch:
            patch.setattr(effects, "canonical_copy", state_mirror.copy_through_json)
            assert validate_effect_program(world, program) == found[count]
    assert found[4] == ()
    assert any("no free floor-plan point for toilet 5" in issue for issue in found[5])


def test_a_model_state_proposal_is_checked_and_stored_as_before(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    namespace = "qa_body_v1"
    descriptor = StateDescriptor(
        namespace=namespace,
        implementation_id="qa-body",
        implementation_version="1",
        schema_version=1,
        fields={"sleep_debt": {"unit": "minute", "public": True}},
    )

    def initial(world: WorldState) -> dict:
        return {"schema_version": 1, "agents": {key: {"sleep_debt": 0} for key in world.agents}}

    def validate(world: WorldState, state: dict) -> None:
        if set(state["agents"]) != set(world.agents) or any(
            row["sleep_debt"] < 0 for row in state["agents"].values()
        ):
            raise ValueError("invalid synthetic debt state")

    monkeypatch.setitem(contracts._CONTRACTS, namespace, (descriptor, validate, initial, None))
    document = ScenarioSpec.load(SMALL_REPLAY).model_dump(mode="json")
    document["model_configs"]["state_contracts"] = {
        "schema_version": 1,
        "namespaces": {
            namespace: {
                "implementation_id": "qa-body",
                "implementation_version": "1",
                "schema_version": 1,
            }
        },
    }
    scenario = ScenarioSpec.model_validate(document)
    results = []
    for make in (state_mirror.canonical_copy, state_mirror.copy_through_json):
        monkeypatch.setattr(contracts, "canonical_copy", make)
        world = WorldState.from_scenario(scenario)
        agents = list(world.agents)
        # Keys out of canonical order: the stored namespace keeps the proposal's order.
        proposed = {
            "schema_version": 1,
            "agents": {key: {"sleep_debt": 3} for key in reversed(agents)},
        }
        contracts.apply_model_proposal(
            world,
            contracts.ModelStateProposal(
                namespace=namespace, model_id="qa-body", minute=0, proposed_state=proposed
            ),
        )
        invalid = copy.deepcopy(proposed)
        invalid["agents"][agents[0]]["sleep_debt"] = -1
        with pytest.raises(ValueError) as error:
            contracts.apply_model_proposal(
                world,
                contracts.ModelStateProposal(
                    namespace=namespace, model_id="qa-body", minute=0, proposed_state=invalid
                ),
            )
        results.append(
            (
                world.canonical_json(),
                list(world.model_state[namespace]["agents"]),
                list(world.agents),
                type(error.value),
                str(error.value),
            )
        )
    assert results[0] == results[1]
    assert results[0][1] == list(reversed(results[0][2]))

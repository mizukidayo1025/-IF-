"""Broken worlds and Python's errors for the core's whole-world validation (#250, #347).

The core validates a world without Python (`sheltersim_core::validate`, which `PlainHost` answers
its validation hooks with): `WorldState.validate` with and without the event history and with an
event in flight, `validate_simulator_extensions`, and the candidate's wrapping of `_txn_validate`.
Its texts must be Python's, so each case here is a valid world with one small change, and what
Python raises for it at each entry point (`ENTRIES`): `[class, str(exc)]`, or None where it passes.

- The valid worlds are `validate_worlds.json` (`BASES`, `validate_worlds_fixture`): small worlds
  run some ticks and exported as the core's loader reads them (`export_world`).
- A case (`Case`) names its base and changes it: `edit` changes the world's document (the JSON of
  `WorldState.to_dict()`), and Python validates `WorldState.from_dict(document, _validate=False)`;
  or `edit_world` changes a live world made from the base, as a pytest does, and the core reads its
  document (`export_world(..., validate=False)`). A case file stores each case's world as the
  changes from its base (`core_texts.variant`, read by `native/core/tests/support/cases.rs`), not
  the whole world.
- Python's own texts are recorded: the Rust twins of the validators are off while a case is
  recorded (`python_validators`), so a rule that a twin answers for runs in Python too.

A lane of #250 adds its cases in a module of its own (`support/core_validate_<area>.py`) with a
builder that returns `cases_fixture(CASES)`, registered in `support/core_fixtures.py` as
`validate_<area>`; `native/core/tests/validate_<area>.rs` checks them with
`support::validate::check_file`. See /home/water7/i263-notes/i250/framework.md.
"""

from __future__ import annotations

import contextlib
import copy
import functools
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from typing import Any

from sheltersim.core.world_export import export_world
from support.core_texts import variant

# The file of the valid worlds, which a case file's worlds refer to.
WORLDS_FILE = "validate_worlds"
# Each entry point and what it calls, in the order a case records them. `in_flight` is recorded
# only for a case with an event in flight.
ENTRIES = ("validate", "validate_tick", "extensions", "candidate", "in_flight")
# The ticks each base world is run before it is exported.
GENERATED_TICKS = 15
SMALL_TICKS = 30


# ---- The valid worlds ----------------------------------------------------------------------------


def _generated() -> Any:
    """The generated Scenario of the core's tick test (10 people, every extension, a second
    service point, an EffectProgram that moves facilities), after `GENERATED_TICKS` ticks."""
    from support.core_tick import generated_run

    world, _, engine = generated_run()
    for _ in range(GENERATED_TICKS):
        engine.step()
    return world


def _small_replay() -> Any:
    """h00_small_replay (WorldState v5, 4 people, no extensions) after `SMALL_TICKS` ticks."""
    from support.core_tick import core_tick_run

    world, _, engine = core_tick_run()
    for _ in range(SMALL_TICKS):
        engine.step()
    return world


# Each valid world by name and the function that builds it.
BASES: dict[str, Callable[[], Any]] = {"generated": _generated, "small_replay": _small_replay}


@functools.cache
def _base_entry(name: str) -> dict[str, Any]:
    return export_world(BASES[name]())


def base_entry(name: str) -> dict[str, Any]:
    """The exported entry of a valid world (a copy)."""
    return copy.deepcopy(_base_entry(name))


def validate_worlds_fixture() -> dict[str, Any]:
    """`validate_worlds.json`: the valid worlds, which every entry point passes."""
    worlds = {}
    for name in BASES:
        entry = base_entry(name)
        entry["expected"] = record(lambda entry=entry: _from_document(entry["world"]))
        assert all(value is None for value in entry["expected"].values()), entry["expected"]
        worlds[name] = entry
    return {"worlds": worlds}


# ---- The catalog the validation reads ---------------------------------------------------------


def validate_catalog() -> dict[str, Any]:
    """`native/core/src/validate/catalog.json`: what `selected_models` and the declared states read
    of the installed code (`sheltersim_core::validate::catalog`): each model of the Model Catalog
    (its domain, whether a factory is installed, its required state contracts, its state schema
    and the bounds of its `behavior.` parameters), the default models and the suites, the installed
    state contracts and the psychology models of each kind."""
    from sheltersim.contracts.bootstrap import ensure_builtin_contracts
    from sheltersim.contracts.policies import (
        CORE_MODEL_SUITES,
        DEFAULT_MODELS,
        MODEL_DEFINITIONS,
        model_catalog,
    )
    from sheltersim.contracts.runtime import _CONTRACTS
    from sheltersim.domain.psychological_state import (
        LIFE_STATE_PSYCHOLOGY_POLICIES,
        MEAN_REVERTING_PSYCHOLOGY_POLICIES,
    )
    from sheltersim.policies.registry import has_installed_factory

    ensure_builtin_contracts()
    catalog = model_catalog()
    models = []
    for model_id, definition in MODEL_DEFINITIONS.items():
        parameters = {
            name: {key: metadata.get(key) for key in ("lower", "upper", "lower_exclusive")}
            for name, metadata in definition.get("parameters", {}).items()
            if name.startswith("behavior.")
        }
        models.append(
            {
                "id": model_id,
                "domain": definition["domain"],
                "factory": has_installed_factory(definition["domain"], model_id),
                "required_state_contracts": definition.get("required_state_contracts", {}),
                "state_schema_version": catalog[model_id]["state_schema_version"],
                "behavior_parameters": parameters,
            }
        )
    contracts = [
        {
            "namespace": namespace,
            "implementation_id": descriptor.implementation_id,
            "implementation_version": descriptor.implementation_version,
            "schema_version": descriptor.schema_version,
            "policy_domain": descriptor.policy_domain,
        }
        for namespace, (descriptor, *_) in sorted(_CONTRACTS.items())
    ]
    return {
        "models": models,
        "default_models": [[domain, model] for domain, model in DEFAULT_MODELS.items()],
        "suites": {
            suite: [[domain, model] for domain, model in models_of.items()]
            for suite, models_of in CORE_MODEL_SUITES.items()
        },
        "state_contracts": contracts,
        "life_state_psychology": sorted(LIFE_STATE_PSYCHOLOGY_POLICIES),
        "mean_reverting_psychology": sorted(MEAN_REVERTING_PSYCHOLOGY_POLICIES),
    }


# ---- Python's errors -----------------------------------------------------------------------------


@contextlib.contextmanager
def python_validators() -> Iterator[None]:
    """The Rust twins of the validators off, as the `python_rules` mark turns them off, and the
    native checks that stand for the Python rules where they pass: each rule runs in Python."""
    from unittest import mock

    from sheltersim import native
    from sheltersim.core import state_mirror
    from sheltersim.core import world as world_module
    from sheltersim.domain import body_state, common_operations

    with contextlib.ExitStack() as stack:
        stack.enter_context(mock.patch.object(state_mirror, "_RUST_VALIDATORS", {}))
        stack.enter_context(mock.patch.object(world_module, "_entity_rules_kernel", None))
        stack.enter_context(mock.patch.object(body_state, "_ROWS_VALID", None))
        stack.enter_context(mock.patch.object(common_operations, "_TICK_VALID", None))
        stack.enter_context(
            mock.patch.object(native, "world_agents_valid", lambda *arguments: False)
        )
        stack.enter_context(
            mock.patch.object(native, "movement_positions_valid", lambda *arguments: False)
        )
        yield


def _outcome(call: Callable[[], Any]) -> list[str] | None:
    try:
        call()
    except Exception as error:  # every class, as Python raised it
        return [type(error).__name__, str(error)]
    return None


def _from_document(document: dict[str, Any]) -> Any:
    from sheltersim.core.world import WorldState

    return WorldState.from_dict(copy.deepcopy(document), _validate=False)


def record(make: Callable[[], Any], in_flight: dict[str, Any] | None = None) -> dict[str, Any]:
    """What each entry point raises for the world `make` builds (a new one for each), with
    Python's validators."""
    from sheltersim.core.state_mirror import _txn_validate
    from sheltersim.domain.simulator_state import validate_simulator_extensions
    from sheltersim.events.models import ScheduledEvent

    calls: dict[str, Callable[[Any], Any]] = {
        "validate": lambda world: world.validate(),
        "validate_tick": lambda world: world.validate(validate_event_history=False),
        "extensions": validate_simulator_extensions,
        "candidate": lambda world: _txn_validate(world, True),
    }
    if in_flight is not None:
        calls["in_flight"] = lambda world: world.validate(
            _in_flight_event=ScheduledEvent(**copy.deepcopy(in_flight))
        )
    expected = {}
    with python_validators():
        for name, call in calls.items():
            world = make()
            expected[name] = _outcome(lambda world=world, call=call: call(world))
    return expected


# ---- Cases ---------------------------------------------------------------------------------------


@dataclass(frozen=True)
class Case:
    """One broken world: a valid world (`base`: a name of `BASES`, or of the bases a case file
    holds itself) and one change to it.

    `edit` changes the world's document in place; `edit_world` changes a live world made from the
    base document, for a change `from_dict` would not read. `in_flight` is an event (its
    `to_dict()`) to validate the world with as in flight, or a function of the changed document
    that gives one."""

    name: str
    base: str
    edit: Callable[[dict[str, Any]], None] | None = None
    edit_world: Callable[[Any], None] | None = None
    in_flight: dict[str, Any] | Callable[[dict[str, Any]], dict[str, Any]] | None = None


def _changed(case: Case, entry: dict[str, Any]) -> tuple[dict[str, Any], Callable[[], Any]]:
    """The case's entry, made from its base's (a copy, changed here), and a function that builds
    its world for Python."""
    if case.edit is not None:
        case.edit(entry["world"])
        document = entry["world"]
        return entry, lambda: _from_document(document)
    if case.edit_world is None:
        return entry, lambda: _from_document(entry["world"])
    edit_world = case.edit_world
    base_document = entry["world"]

    def make() -> Any:
        world = _from_document(base_document)
        edit_world(world)
        return world

    made = copy.deepcopy(entry)
    made["world"] = export_world(make(), validate=False)["world"]
    return made, make


def cases_fixture(
    cases: list[Case], bases: dict[str, Callable[[], Any]] | None = None
) -> dict[str, Any]:
    """A case file: each case's world as the changes from its base, and what Python raises for it
    at each entry point. A base is a world of `validate_worlds.json` (`BASES`), or one of the
    file's own `bases` (a name and a function that builds the live world), which the file then
    holds whole."""
    own = {name: export_world(build()) for name, build in (bases or {}).items()}
    names = [case.name for case in cases]
    assert len(names) == len(set(names)), "a case name is used twice"
    assert not set(names) & set(own), "a case has the name of a base"
    worlds: dict[str, Any] = {name: copy.deepcopy(entry) for name, entry in own.items()}
    out: list[dict[str, Any]] = []
    for case in cases:
        base = copy.deepcopy(own[case.base]) if case.base in own else base_entry(case.base)
        entry, make = _changed(case, copy.deepcopy(base))
        in_flight = case.in_flight(entry["world"]) if callable(case.in_flight) else case.in_flight
        changes = variant(case.base, base, entry, dotted=True)
        worlds[case.name] = changes if case.base in own else {"file": WORLDS_FILE, **changes}
        item: dict[str, Any] = {"name": case.name, "world": case.name}
        if in_flight is not None:
            item["in_flight"] = in_flight
        item["expected"] = record(make, in_flight)
        out.append(item)
    return {"worlds": worlds, "cases": out}


# ---- The cases of the entry points (#347, PR 1) --------------------------------------------------


def _set(*path: Any, value: Any) -> Callable[[dict[str, Any]], None]:
    """An edit that sets the value at a path of the document."""

    def edit(document: dict[str, Any]) -> None:
        node = document
        for key in path[:-1]:
            node = node[key]
        node[path[-1]] = value

    return edit


def _drop(*path: Any) -> Callable[[dict[str, Any]], None]:
    """An edit that removes a key of a dict of the document."""

    def edit(document: dict[str, Any]) -> None:
        node = document
        for key in path[:-1]:
            node = node[key]
        del node[path[-1]]

    return edit


def _set_attribute(name: str, value: Any) -> Callable[[Any], None]:
    """An edit of a live world that sets one of its attributes."""

    def edit(world: Any) -> None:
        setattr(world, name, value)

    return edit


ENTRY_CASES = [
    # from_dict refuses another version before it builds the world.
    Case(
        "schema_version_7",
        "generated",
        edit_world=_set_attribute("schema_version", 7),
        in_flight=lambda document: document["event_queue"][0],
    ),
    Case("schema_version_4_small", "small_replay", edit_world=_set_attribute("schema_version", 4)),
    Case(
        "shared_operations_without_contract",
        "generated",
        edit=_drop("model_configs", "shared_operations"),
    ),
]


def validate_entry_fixture() -> dict[str, Any]:
    """`validate_entry.json`: the cases of the entry points' own rules (the schema version, the
    signed contract of the shared operations)."""
    return cases_fixture(ENTRY_CASES)

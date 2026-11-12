"""resource_ledger_v1 owned by Rust, with its operations (docs/rust-port/lanes/m4-resources.md).

The operation has no Python rule left (lane L10, `kernel_only`). What it writes and says, against
what the deleted Python rule gave (the ledger and resources after each sequence of operations, each
`ResourceLedgerError` text, the outcomes of random sequences with refusals), is checked without
Python in `native/core/tests/resource_ledger_ops.rs`, on the same world
(`support/core_fixtures_reads.py`, #225). Here are the entries: each still reaches the kernel and
gives its answer, the texts with Python's `repr` of a payload value, what taints the row, and that
the check takes nothing the Python rules refuse.
"""

from __future__ import annotations

import copy
import json
import random
from types import SimpleNamespace
from typing import Any

import pytest
from support.core_fixtures_reads import ledger_world

from sheltersim import native
from sheltersim.core.checkpoint import Checkpoint
from sheltersim.core.checkpoint_cache import canonical_bytes
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import rust_validated
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain import resources
from sheltersim.domain.resources import (
    RESOURCE_LEDGER_KEY as KEY,
)
from sheltersim.domain.resources import (
    RESOURCE_METRIC_FIELDS,
    ResourceLedgerError,
    apply_resource_operation,
    reservation_id_was_used,
    resource_metric_values,
    validate_resource_ledger,
    validate_resource_ledger_state,
)
from sheltersim.events.models import EventKind, ScheduledEvent

SOURCE = "ASM-TEST-RESOURCE-OPERATION"
SEQUENCE = [0]


@pytest.fixture(autouse=True)
def _trees(keep_trees: Any) -> None:
    """These tests pin the tree the kernel writes beside the row, key order and all: the
    namespace keeps its tree here (support/world.py keep_trees)."""
    keep_trees(KEY)


def _world() -> WorldState:
    SEQUENCE[0] = 0
    return ledger_world()


def _event(operation: str, source: Any = SOURCE, event_id: Any = None, **payload: Any) -> Any:
    SEQUENCE[0] += 1
    return ScheduledEvent(
        event_id=event_id or f"op-{SEQUENCE[0]}",
        kind=EventKind.EXOGENOUS,
        due_minute=0,
        priority=0,
        sequence=SEQUENCE[0],
        event_type=f"resource_{operation}",
        payload={"warehouse_id": "warehouse-main", **payload},
        source_id=source,
    )


def _kernel() -> Any:
    return native.rules("resources")


def _text(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False)


def _apply(world: WorldState, event: Any, operation: str) -> None:
    """The operation, through its entry; the row must stay the tree."""
    apply_resource_operation(world, event, operation)
    _assert_row_is_the_tree(world)


def _assert_row_is_the_tree(world: WorldState) -> None:
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == canonical_bytes(world.model_state[KEY])


def _operate(world: WorldState) -> str:
    agent = sorted(world.agents)[0]
    meal = {"resource_id": "food_meal_units", "agent_id": agent}
    world.minute_index = 1
    _apply(world, _event("delivery", resource_id="water_ml", quantity=500), "delivery")
    world.minute_index = 2
    _apply(world, _event("reserve", reservation_id="r-1", quantity=2, **meal), "reserve")
    world.minute_index = 3
    _apply(world, _event("consume", reservation_id="r-1", quantity=1, **meal), "consume")
    world.minute_index = 4
    _apply(world, _event("reserve", reservation_id="r-2", quantity=1, **meal), "reserve")
    _apply(world, _event("release", reservation_id="r-2", **meal), "release")
    world.minute_index = 9
    _apply(world, _event("loss", resource_id="water_ml", quantity=200), "loss")
    return agent


def test_each_operation_writes_what_the_python_rule_wrote() -> None:
    """The entry of the operation (the core's test of the same name has the digest of what it
    wrote)."""
    world = _world()
    _operate(world)
    account = world.model_state[KEY]["accounts"]["food_meal_units"]
    assert account["reservations"]["r-1"]["quantity"] == 1
    assert world.model_state[KEY]["used_reservation_ids"] == ["r-1", "r-2"]
    assert world.resources == {"water_ml": 1_300, "food_meal_units": 3}
    validate_resource_ledger(
        world.model_state[KEY], world.resources, set(world.agents), set(world.zones)
    )


def test_the_namespace_is_not_written_whole_and_the_resources_are_a_new_dict() -> None:
    world = _world()
    mirror = world._core.mirror(KEY)
    tree = world.model_state[KEY]
    whole_writes = mirror.stats()["whole_writes"]
    held = world.resources
    before = dict(held)
    _operate(world)
    assert mirror.stats()["whole_writes"] == whole_writes
    assert world.model_state[KEY] is tree
    assert world.resources is not held
    assert held == before
    # The new dict is the tree of the core's row (PR 5-2 of #152); the old one is a plain dict.
    resources_mirror = world._core.mirror("resources")
    assert resources_mirror.holds(world.resources)
    assert resources_mirror.row_json() == canonical_bytes(world.resources)
    held["water_ml"] = -1
    assert resources_mirror.row_json() == canonical_bytes(world.resources)


def test_the_public_rule_runs_the_kernel() -> None:
    world = _world()
    world.minute_index = 1
    tree = world.model_state[KEY]
    apply_resource_operation(
        world, _event("delivery", resource_id="water_ml", quantity=5), "delivery"
    )
    assert world.model_state[KEY] is tree


# name -> the event, the operation, and the Python rule's text: those with Python's `repr` of a
# payload value, which the entry writes (the core's test of the same name has every refusal).
REFUSED = {
    "a warehouse that is not a str": (
        lambda agent: ScheduledEvent(
            event_id="int-warehouse",
            kind=EventKind.EXOGENOUS,
            due_minute=0,
            priority=0,
            sequence=90_002,
            event_type="resource_delivery",
            payload={"warehouse_id": 7, "resource_id": "water_ml", "quantity": 1},
            source_id=SOURCE,
        ),
        "delivery",
        "unknown resource warehouse ID 7",
    ),
    "a resource that is not a str": (
        lambda agent: _event("delivery", resource_id=None, quantity=1),
        "delivery",
        "unknown resource ID None",
    ),
    "an unknown operation": (
        lambda agent: _event("delivery", resource_id="water_ml", quantity=1),
        "steal",
        "unknown resource operation 'steal'",
    ),
}


@pytest.mark.parametrize("case", sorted(REFUSED))
def test_a_refusal_changes_nothing_and_has_the_python_text(case: str) -> None:
    build, operation, text = REFUSED[case]
    world = _world()
    agent = _operate(world)
    event = build(agent)
    before = world.canonical_json()
    with pytest.raises(ResourceLedgerError) as raised:
        apply_resource_operation(world, event, operation)
    assert type(raised.value) is ResourceLedgerError
    assert str(raised.value) == text
    assert world.canonical_json() == before
    _assert_row_is_the_tree(world)


def test_without_a_ledger_the_operation_says_so() -> None:
    world = _world()
    del world.model_state[KEY]
    with pytest.raises(ResourceLedgerError) as raised:
        apply_resource_operation(
            world, _event("delivery", resource_id="water_ml", quantity=1), "delivery"
        )
    assert str(raised.value) == "resource ledger is not enabled for this scenario"


@pytest.mark.parametrize(
    ("source", "event_id"),
    [("not a stable id", "op-a"), (SOURCE, "bad id"), ("x" * 65, "op-b")],
)
def test_ids_an_event_does_not_take_are_refused(source: str, event_id: str) -> None:
    """`ScheduledEvent` turns these IDs down, so only another object can bring them."""
    world = _world()
    world.minute_index = 1
    event = SimpleNamespace(
        payload={"warehouse_id": "warehouse-main", "resource_id": "water_ml", "quantity": 1},
        source_id=source,
        event_id=event_id,
    )
    before = world.canonical_json()
    with pytest.raises(KernelRefusal, match="not a stable ID"):
        apply_resource_operation(world, event, "delivery")
    assert world.canonical_json() == before


def test_the_metric_columns_and_the_used_ids() -> None:
    world = _world()
    _operate(world)
    columns = resource_metric_values(world)
    assert list(columns) == list(RESOURCE_METRIC_FIELDS)
    assert columns == {
        "water_ml_initial": 1_000,
        "water_ml_delivered": 500,
        "water_ml_served": 0,
        "water_ml_losses": 200,
        "water_ml_remaining": 1_300,
        "water_ml_reserved": 0,
        "food_meal_units_initial": 4,
        "food_meal_units_delivered": 0,
        "food_meal_units_served": 1,
        "food_meal_units_losses": 0,
        "food_meal_units_remaining": 3,
        "food_meal_units_reserved": 1,
    }
    assert reservation_id_was_used(world, "r-1") is True
    assert reservation_id_was_used(world, "r-404") is False
    # The Python rule answers for anything but a str, as `in` on a list does.
    assert reservation_id_was_used(world, 3) is False


def _validate_state(world: WorldState) -> None:
    validate_resource_ledger_state(
        world.model_state[KEY],
        world.resources,
        set(world.agents),
        set(world.zones),
        minute_index=world.minute_index,
    )


# name -> what it does, and the text the Python rules say.
TAMPERING = {
    "remaining": (
        lambda world: world.model_state[KEY]["accounts"]["water_ml"].__setitem__("remaining", 1),
        "accounting does not balance",
    ),
    "a negative balance": (
        lambda world: world.model_state[KEY]["accounts"]["water_ml"].__setitem__("losses", -1),
        "must be non-negative",
    ),
    "resources": (
        lambda world: world.resources.__setitem__("water_ml", 1),
        "does not match ledger remaining",
    ),
    "a whole assignment of resources": (
        lambda world: setattr(world, "resources", {"water_ml": 1}),
        "exactly the ledger resource IDs",
    ),
    "a newest audit record": (
        lambda world: world.model_state[KEY]["audit"][-1].__setitem__("remaining", 7),
        "balance does not match audit",
    ),
    "a repeated reservation ID": (
        lambda world: world.model_state[KEY]["used_reservation_ids"].append("r-1"),
        "must be unique",
    ),
    "a reservation ID not a str": (
        lambda world: world.model_state[KEY]["used_reservation_ids"].append(3),
        "reservation IDs are invalid",
    ),
    "a reservation's quantity": (
        lambda world: world.model_state[KEY]["accounts"]["food_meal_units"]["reservations"][
            "r-1"
        ].__setitem__("quantity", 5),
        "reserved total does not match",
    ),
    "a reservation's person": (
        lambda world: world.model_state[KEY]["accounts"]["food_meal_units"]["reservations"][
            "r-1"
        ].__setitem__("agent_id", "nobody"),
        "reservation identity is invalid",
    ),
    "the schema": (
        lambda world: world.model_state[KEY].__setitem__("schema_version", 2),
        "unsupported resource ledger schema",
    ),
    "the warehouse": (
        lambda world: world.model_state[KEY].__setitem__("warehouse_id", "bad id"),
        "warehouse_id is invalid",
    ),
    "the zone": (
        lambda world: world.model_state[KEY].__setitem__("zone_id", "nowhere"),
        "zone_id is invalid or unknown",
    ),
    "a source": (
        lambda world: world.model_state[KEY]["accounts"]["water_ml"]["source_ids"].append("bad id"),
        "source_ids are invalid",
    ),
    "a repeated source": (
        lambda world: world.model_state[KEY]["accounts"]["water_ml"]["source_ids"].append(SOURCE),
        "source_ids must be unique",
    ),
    "an extra field": (
        lambda world: world.model_state[KEY].__setitem__("extra", 1),
        "fields do not match v1",
    ),
    "a whole namespace": (
        lambda world: world.model_state.__setitem__(KEY, {"schema_version": 1}),
        "fields do not match v1",
    ),
}


@pytest.mark.parametrize("case", sorted(TAMPERING))
def test_each_write_is_stopped_with_the_python_text(case: str) -> None:
    tamper, text = TAMPERING[case]
    world = _world()
    _operate(world)
    assert rust_validated(world, KEY)
    world.validate(validate_event_history=False)
    tamper(world)
    assert not rust_validated(world, KEY)
    with pytest.raises(StateInvariantError, match=text):
        world.validate(validate_event_history=False)
    # The Python rules' check of the ledger turned these down with the same text; the kernels
    # say only that they refuse.
    before = _text(world.model_state[KEY]) + _text(world.resources)
    with pytest.raises(KernelRefusal):
        resource_metric_values(world)
    with pytest.raises(KernelRefusal):
        apply_resource_operation(
            world, _event("delivery", resource_id="water_ml", quantity=1), "delivery"
        )
    assert _text(world.model_state[KEY]) + _text(world.resources) == before


def test_balances_that_add_up_but_are_not_the_audit_trail_s_are_refused() -> None:
    world = _world()
    _operate(world)
    account = world.model_state[KEY]["accounts"]["water_ml"]
    account["served"] += 1
    account["remaining"] -= 1
    world.resources["water_ml"] -= 1
    assert not rust_validated(world, KEY)
    with pytest.raises(ResourceLedgerError, match="balance does not match audit"):
        _validate_state(world)


def test_a_bool_in_the_resources_is_refused() -> None:
    world = _world()
    _operate(world)
    world.resources["food_meal_units"] = True
    assert not rust_validated(world, KEY)
    with pytest.raises(ResourceLedgerError, match="does not match ledger remaining"):
        _validate_state(world)


def test_a_record_of_an_older_minute_goes_to_python_and_passes_there() -> None:
    """Python's check of a tick reads only the newest records, so it says nothing of this one."""
    world = _world()
    _operate(world)
    ledger = world.model_state[KEY]
    newest = dict(ledger["audit"][-1])
    ledger["audit"].append({**newest, "minute": 0, "source_ids": list(newest["source_ids"])})
    world.minute_index = 30
    assert not rust_validated(world, KEY)
    _validate_state(world)


def test_a_write_past_the_table_is_read_by_the_python_rules() -> None:
    world = _world()
    _operate(world)
    good = json.loads(json.dumps(world.model_state[KEY]))
    good["accounts"]["water_ml"]["remaining"] = 1
    dict.__setitem__(world.model_state, KEY, good)
    assert not rust_validated(world, KEY)
    with pytest.raises(native.Unsupported):
        _kernel().metric_values(world)
    with pytest.raises(native.Unsupported):
        _kernel().apply_operation(
            world, _event("delivery", resource_id="water_ml", quantity=1), "delivery"
        )
    with pytest.raises(native.Unsupported):
        _kernel().reservation_id_was_used(world, "r-1")
    with pytest.raises(StateInvariantError, match="accounting does not balance"):
        world.validate()


def test_a_tainted_row_comes_back_with_the_python_rule_s_write() -> None:
    world = _world()
    _operate(world)
    world.model_state[KEY]["audit"][2]["served"] = 1.5
    assert world._core.mirror(KEY).tainted is not None
    mirror = world._core.mirror(KEY)
    world.model_state[KEY]["audit"][2]["served"] = 0
    assert mirror.refit()
    _assert_row_is_the_tree(world)


def test_a_forwarded_append_keeps_the_row() -> None:
    world = _world()
    _operate(world)
    record = dict(world.model_state[KEY]["audit"][-1])
    world.model_state[KEY]["audit"].append(
        {**record, "minute": 9, "event_id": "by-hand", "operation": "delivery", "agent_id": None}
    )
    _assert_row_is_the_tree(world)


def test_a_deep_copy_and_a_restore_have_their_own_rows() -> None:
    world = _world()
    _operate(world)
    copied = copy.deepcopy(world)
    copied.minute_index = 10
    before = world.canonical_json()
    _apply(copied, _event("delivery", resource_id="water_ml", quantity=1), "delivery")
    assert world.canonical_json() == before
    _assert_row_is_the_tree(world)

    restored = WorldState.from_dict(json.loads(world.canonical_json()))
    assert rust_validated(restored, KEY)
    restored.minute_index = 10
    _apply(restored, _event("delivery", resource_id="water_ml", quantity=1), "delivery")
    checkpoint = Checkpoint.capture(restored)
    assert (
        checkpoint.state_hash
        == Checkpoint.capture(
            WorldState.from_dict(json.loads(restored.canonical_json()))
        ).state_hash
    )


def test_the_save_check_still_replays_the_audit_trail() -> None:
    """The kernel answers for a tick; only the replay sees a change that left the newest alone."""
    world = _world()
    _operate(world)
    world.validate(validate_event_history=False)
    world.model_state[KEY]["audit"][2]["delivered"] += 1
    with pytest.raises(StateInvariantError, match="audit after-state does not match replay"):
        world.validate()


def test_the_check_takes_nothing_python_refuses_on_random_writes() -> None:
    randomizer = random.Random(7)
    values: list[Any] = [-1, 0, 1, 2, 3, 500, 1_300, 10**30, True, 1.0, None, "x", "bad id", "r-1"]
    accepted = refused = 0
    for _ in range(600):
        world = _world()
        _operate(world)
        ledger = world.model_state[KEY]
        account = ledger["accounts"][randomizer.choice(["water_ml", "food_meal_units"])]
        target = randomizer.randrange(9)
        value = randomizer.choice(values)
        if target == 0:
            account[randomizer.choice(list(resources._BALANCE_FIELDS))] = value
        elif target == 1:
            world.resources[randomizer.choice(["water_ml", "food_meal_units"])] = value
        elif target == 2:
            ledger["used_reservation_ids"].append(value)
        elif target == 3:
            ledger["accounts"]["food_meal_units"]["reservations"]["r-1"][
                randomizer.choice(["reservation_id", "resource_id", "agent_id", "quantity"])
            ] = value
        elif target == 4:
            account["source_ids"].append(value)
        elif target == 5:
            ledger["audit"][-1][randomizer.choice(list(resources._AUDIT_FIELDS))] = value
        elif target == 6:
            ledger[randomizer.choice(["schema_version", "warehouse_id", "zone_id"])] = value
        elif target == 7:
            ledger["accounts"]["food_meal_units"]["reservations"][str(value)] = {
                "reservation_id": str(value),
                "resource_id": "food_meal_units",
                "agent_id": sorted(world.agents)[0],
                "quantity": 1,
            }
        else:
            ledger["audit"].append(value)
        if rust_validated(world, KEY):
            accepted += 1
            _validate_state(world)
        else:
            refused += 1
    assert accepted > 0
    assert refused > 300

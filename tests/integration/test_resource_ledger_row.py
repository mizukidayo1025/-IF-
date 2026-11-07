"""resource_ledger_v1 owned by Rust, with its operations (docs/rust-port/lanes/m4-resources.md).

The operation has no Python rule left (lane L10, `kernel_only`). Here, what it wrote and said is
checked against what the Python rule gave, written down from it before it went: the ledger and
resources after each sequence of operations (`_digest`), each `ResourceLedgerError` text, and the
outcomes of random sequences with refusals. And what taints the row, and that the check takes
nothing the Python rules refuse.
"""

from __future__ import annotations

import copy
import hashlib
import json
import random
from types import SimpleNamespace
from typing import Any

import pytest
from support.paths import SMALL_REPLAY

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
from sheltersim.experiments.scenario import ScenarioSpec

SOURCE = "ASM-TEST-RESOURCE-OPERATION"
SEQUENCE = [0]


def _world() -> WorldState:
    SEQUENCE[0] = 0
    document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    document["schema_version"] = 3
    document["duration_minutes"] = 300
    document["scheduled_events"] = []
    document["initial_stock"] = {
        "schema_version": 1,
        "warehouse_id": "warehouse-main",
        "zone_id": "sleeping",
        "water_ml": 1_000,
        "food_meal_units": 4,
        "source_ids": ["ASM-TEST-INITIAL-STOCK"],
    }
    return WorldState.from_scenario(ScenarioSpec.model_validate(document))


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


def _digest(world: Any) -> str:
    """A digest of the ledger and the resources, to pin what the Python rule wrote."""
    text = _text(world.model_state[KEY]) + _text(world.resources)
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:16]


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


def _whole_reservation_and_a_new_source(world: WorldState, agent: str) -> None:
    meal = {"resource_id": "food_meal_units", "agent_id": agent}
    _apply(world, _event("consume", reservation_id="r-1", quantity=1, **meal), "consume")
    _apply(
        world,
        _event("delivery", source="ASM-ANOTHER-SOURCE", resource_id="water_ml", quantity=1),
        "delivery",
    )


def test_each_operation_writes_what_the_python_rule_wrote() -> None:
    world = _world()
    _operate(world)
    assert _digest(world) == "4077215d5a94dd42"
    account = world.model_state[KEY]["accounts"]["food_meal_units"]
    assert account["reservations"]["r-1"]["quantity"] == 1
    assert world.model_state[KEY]["used_reservation_ids"] == ["r-1", "r-2"]
    assert world.resources == {"water_ml": 1_300, "food_meal_units": 3}
    validate_resource_ledger(
        world.model_state[KEY], world.resources, set(world.agents), set(world.zones)
    )


def test_a_consume_of_the_whole_reservation_and_a_new_source() -> None:
    world = _world()
    agent = _operate(world)
    _whole_reservation_and_a_new_source(world, agent)
    assert _digest(world) == "0e81c8c83d0c3d2d"
    assert world.model_state[KEY]["accounts"]["food_meal_units"]["reservations"] == {}
    assert world.model_state[KEY]["accounts"]["water_ml"]["source_ids"][-1] == "ASM-ANOTHER-SOURCE"
    assert world.model_state[KEY]["audit"][-1]["source_ids"][-1] == "ASM-ANOTHER-SOURCE"


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


def test_the_public_rule_runs_the_kernel() -> None:
    world = _world()
    world.minute_index = 1
    tree = world.model_state[KEY]
    apply_resource_operation(
        world, _event("delivery", resource_id="water_ml", quantity=5), "delivery"
    )
    assert world.model_state[KEY] is tree


# name -> the event, the operation, and the Python rule's text.
REFUSED = {
    "an unknown warehouse": (
        lambda agent: ScheduledEvent(
            event_id="bad-warehouse",
            kind=EventKind.EXOGENOUS,
            due_minute=0,
            priority=0,
            sequence=90_001,
            event_type="resource_delivery",
            payload={"warehouse_id": "elsewhere", "resource_id": "water_ml", "quantity": 1},
            source_id=SOURCE,
        ),
        "delivery",
        "unknown resource warehouse ID 'elsewhere'",
    ),
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
    "an unknown resource": (
        lambda agent: _event("delivery", resource_id="soap", quantity=1),
        "delivery",
        "unknown resource ID 'soap'",
    ),
    "a resource that is not a str": (
        lambda agent: _event("delivery", resource_id=None, quantity=1),
        "delivery",
        "unknown resource ID None",
    ),
    "a quantity of zero": (
        lambda agent: _event("delivery", resource_id="water_ml", quantity=0),
        "delivery",
        "resource quantity must be a positive integer",
    ),
    "a negative quantity past 64 bits": (
        lambda agent: _event("loss", resource_id="water_ml", quantity=-(10**30)),
        "loss",
        "resource quantity must be a positive integer",
    ),
    "a bool quantity": (
        lambda agent: _event("delivery", resource_id="water_ml", quantity=True),
        "delivery",
        "resource quantity must be a positive integer",
    ),
    "a float quantity": (
        lambda agent: _event("delivery", resource_id="water_ml", quantity=1.0),
        "delivery",
        "resource quantity must be a positive integer",
    ),
    "no source": (
        lambda agent: _event("delivery", source=None, resource_id="water_ml", quantity=1),
        "delivery",
        "resource events require a source_id",
    ),
    "no source before a bad quantity": (
        lambda agent: _event("delivery", source=None, resource_id="water_ml", quantity=0),
        "delivery",
        "resource events require a source_id",
    ),
    "other payload fields": (
        lambda agent: _event("delivery", resource_id="water_ml", quantity=1, extra=1),
        "delivery",
        "resource delivery payload fields do not match",
    ),
    "a missing payload field": (
        lambda agent: _event("release", resource_id="food_meal_units", reservation_id="r-1"),
        "release",
        "resource release payload fields do not match",
    ),
    "a loss past what is free": (
        lambda agent: _event("loss", resource_id="food_meal_units", quantity=4),
        "loss",
        "resource loss exceeds unreserved remaining inventory",
    ),
    "a loss past 64 bits": (
        lambda agent: _event("loss", resource_id="water_ml", quantity=10**30),
        "loss",
        "resource loss exceeds unreserved remaining inventory",
    ),
    "a used ID": (
        lambda agent: _event(
            "reserve",
            resource_id="food_meal_units",
            agent_id=agent,
            reservation_id="r-1",
            quantity=1,
        ),
        "reserve",
        "resource reservation ID 'r-1' was already used",
    ),
    "a used ID before a reservation past 64 bits": (
        lambda agent: _event(
            "reserve",
            resource_id="food_meal_units",
            agent_id=agent,
            reservation_id="r-2",
            quantity=10**30,
        ),
        "reserve",
        "resource reservation ID 'r-2' was already used",
    ),
    "a reservation past what is free": (
        lambda agent: _event(
            "reserve",
            resource_id="food_meal_units",
            agent_id=agent,
            reservation_id="r-9",
            quantity=9,
        ),
        "reserve",
        "resource reservation exceeds available inventory",
    ),
    "a reservation past 64 bits": (
        lambda agent: _event(
            "reserve",
            resource_id="food_meal_units",
            agent_id=agent,
            reservation_id="r-9",
            quantity=10**30,
        ),
        "reserve",
        "resource reservation exceeds available inventory",
    ),
    "a bad reservation ID": (
        lambda agent: _event(
            "reserve",
            resource_id="food_meal_units",
            agent_id=agent,
            reservation_id="r 9",
            quantity=1,
        ),
        "reserve",
        "resource reservation_id must be a stable ID",
    ),
    "someone unknown": (
        lambda agent: _event(
            "reserve",
            resource_id="food_meal_units",
            agent_id="nobody",
            reservation_id="r 9",
            quantity=1,
        ),
        "reserve",
        "unknown resource Agent ID 'nobody'",
    ),
    "a bad quantity before someone unknown": (
        lambda agent: _event(
            "consume",
            resource_id="food_meal_units",
            agent_id="nobody",
            reservation_id="r-1",
            quantity=0,
        ),
        "consume",
        "resource quantity must be a positive integer",
    ),
    "a reservation not there": (
        lambda agent: _event(
            "release", resource_id="food_meal_units", agent_id=agent, reservation_id="r-2"
        ),
        "release",
        "unknown resource reservation 'r-2'",
    ),
    "a consume past the reservation": (
        lambda agent: _event(
            "consume",
            resource_id="food_meal_units",
            agent_id=agent,
            reservation_id="r-1",
            quantity=2,
        ),
        "consume",
        "resource consume exceeds the reservation",
    ),
    "a consume past 64 bits": (
        lambda agent: _event(
            "consume",
            resource_id="food_meal_units",
            agent_id=agent,
            reservation_id="r-1",
            quantity=10**30,
        ),
        "consume",
        "resource consume exceeds the reservation",
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


def test_someone_else_s_reservation_has_the_python_text() -> None:
    world = _world()
    _operate(world)
    other = sorted(world.agents)[1]
    event = _event("release", resource_id="food_meal_units", agent_id=other, reservation_id="r-1")
    with pytest.raises(ResourceLedgerError) as raised:
        apply_resource_operation(world, event, "release")
    assert str(raised.value) == "resource reservation belongs to a different Agent"


def test_without_a_ledger_the_operation_says_so() -> None:
    world = _world()
    del world.model_state[KEY]
    with pytest.raises(ResourceLedgerError) as raised:
        apply_resource_operation(
            world, _event("delivery", resource_id="water_ml", quantity=1), "delivery"
        )
    assert str(raised.value) == "resource ledger is not enabled for this scenario"


def test_a_delivery_past_64_bits_is_refused_with_nothing_changed() -> None:
    """The Python rule took it into a ledger of ints past 64 bits, which the row doesn't hold."""
    world = _world()
    _operate(world)
    before = world.canonical_json()
    with pytest.raises(KernelRefusal, match="past 64 bits"):
        apply_resource_operation(
            world, _event("delivery", resource_id="water_ml", quantity=10**30), "delivery"
        )
    assert world.canonical_json() == before


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


def test_an_older_minute_is_refused() -> None:
    world = _world()
    _operate(world)
    world.minute_index = 4
    with pytest.raises(native.Unsupported):
        _kernel().apply_operation(
            world, _event("delivery", resource_id="water_ml", quantity=1), "delivery"
        )


def _random_sequences() -> tuple[str, int, int]:
    """Random operations with refusals: a digest of each outcome (the text of a refusal, else the
    ledger and resources after it), and how many ran and were refused."""
    randomizer = random.Random(20261101)
    ran = refused = 0
    outcomes = hashlib.sha256()
    for _ in range(25):
        world = _world()
        people = sorted(world.agents)[:3]
        reservations = [f"r-{index}" for index in range(12)]
        for minute in range(1, 60):
            world.minute_index = minute
            operation = randomizer.choice(["delivery", "loss", "reserve", "release", "consume"])
            payload: dict[str, Any] = {
                "resource_id": randomizer.choice(["water_ml", "food_meal_units"])
            }
            if operation != "release":
                payload["quantity"] = randomizer.choice([1, 1, 2, 3, 50, 2000])
            if operation in {"reserve", "release", "consume"}:
                payload["agent_id"] = randomizer.choice(people)
                payload["reservation_id"] = randomizer.choice(reservations)
            event = _event(
                operation, source=randomizer.choice([SOURCE, "ASM-SECOND", "ASM-THIRD"]), **payload
            )
            before = world.canonical_json()
            try:
                apply_resource_operation(world, event, operation)
            except ResourceLedgerError as error:
                assert world.canonical_json() == before
                outcomes.update(f"refused {error}\n".encode())
                refused += 1
                continue
            outcomes.update(f"ran {_digest(world)}\n".encode())
            ran += 1
        _assert_row_is_the_tree(world)
    return outcomes.hexdigest()[:16], ran, refused


def test_random_sequences_with_refusals_are_the_python_rule_s() -> None:
    assert _random_sequences() == ("878738072d3482ee", 720, 755)


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

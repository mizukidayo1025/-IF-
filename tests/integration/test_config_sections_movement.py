"""The configs' sections of lane 7-5a are kept by the stamp of model_configs (Issue #152).

Movement, routing, the queue policy, the overlay's schema, the resource service, the population's
needs and the simulator extensions: each section is parsed at its first read after model_configs
changed and kept for that stamp (native/src/core/configs.rs). A read site raises what it raised
when it read the configs at every call: a refusal (U) with its reason, the queue policy's
ValueError with its text, and an error it relays (R) as Python raises it, with the same class and
text, from a second evaluation. `sheltersim_native.config_section` reads a section through the read
site's own accessor; `config_parses` counts the parses of the sections.
"""

from __future__ import annotations

from typing import Any

import pytest
import sheltersim_native

from sheltersim import native
from sheltersim.core.state_mirror import field_stamp
from sheltersim.core.world import WorldState
from sheltersim.policies.queueing import create_queue_policy
from sheltersim.policies.space import ZoneRoute


def _read(world: WorldState, name: str, key: str | None = None) -> Any:
    return sheltersim_native.config_section(world, name, key)


@pytest.fixture
def world(generated_scenario: Any) -> WorldState:
    return WorldState.from_scenario(generated_scenario)


def _routing(capacities: dict[str, Any]) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "source_type": "assumption",
        "source_id": "ASM-A05-ROUTE-EDIT-V1",
        "max_agents_in_transit_by_edge": capacities,
    }


# Each section with its key, and an edit of the configs that changes what the read gives.
SECTIONS = [
    ("movement", None, lambda c: c["movement_model"].__setitem__("maximum_density_slowdown", 3.0)),
    ("route_capacities", None, lambda c: c.__setitem__("layout_routing", _routing({"e1": 2}))),
    (
        "transit_capacities",
        None,
        lambda c: c.__setitem__("layout_routing", _routing({"e1": 2})),
    ),
    (
        "facility_queues",
        None,
        lambda c: c["facility_queues"].__setitem__("retry_delay_minutes", 30),
    ),
    ("overlay_schema", None, lambda c: c["geometry_overlay"].__setitem__("schema_version", 3)),
    ("overlay_rooms", None, lambda c: c["geometry_overlay"].__setitem__("schema_version", 2)),
    (
        "resource_service",
        None,
        lambda c: c["resource_service"].__setitem__("water_staff_skill", "x"),
    ),
    (
        "accessibility_needs",
        "A001",
        lambda c: c["population"]["people"]["A001"].__setitem__("accessibility_needs", ["quiet"]),
    ),
    ("extensions_enabled", None, lambda c: c["simulator_extensions"].__setitem__("enabled", False)),
]


@pytest.mark.parametrize(("name", "key", "edit"), SECTIONS, ids=[s[0] for s in SECTIONS])
def test_a_section_is_parsed_once_for_a_stamp(
    world: WorldState, name: str, key: str | None, edit: Any
) -> None:
    # The first read may find the section parsed already: building the world reads some.
    first = _read(world, name, key)
    before = sheltersim_native.config_parses()
    stamp = field_stamp(world, "model_configs")
    assert _read(world, name, key) == first
    assert sheltersim_native.config_parses() == before
    # Reading changes no stamp; a write in place gives a new one, and the next read parses again.
    assert field_stamp(world, "model_configs") == stamp
    edit(world.model_configs)
    assert field_stamp(world, "model_configs") != stamp
    second = _read(world, name, key)
    assert second != first
    assert sheltersim_native.config_parses() == before + 1
    assert _read(world, name, key) == second
    assert sheltersim_native.config_parses() == before + 1


def test_configs_no_slot_follows_are_parsed_at_every_read(world: WorldState) -> None:
    world.model_configs = dict(world.model_configs)
    assert field_stamp(world, "model_configs") is None
    before = sheltersim_native.config_parses()
    for _ in range(2):
        assert _read(world, "movement")[0] is True
        assert _read(world, "facility_queues")[0] == "need_weighted"
    assert sheltersim_native.config_parses() == before + 4


def _same_error(read: Any, python: Any) -> BaseException:
    """What read raises, twice, is what python raises: the same class and text."""
    with pytest.raises(Exception) as expected:
        python()
    for _ in range(2):
        with pytest.raises(Exception) as raised:
            read()
        assert type(raised.value) is type(expected.value)
        assert str(raised.value) == str(expected.value)
    return raised.value


def test_a_relayed_error_is_python_s_own_at_every_read(world: WorldState) -> None:
    configs = world.model_configs
    configs["layout_routing"] = ["not", "a", "dict"]
    error = _same_error(
        lambda: _read(world, "transit_capacities"),
        lambda: configs.get("layout_routing", {}).get("max_agents_in_transit_by_edge", {}),
    )
    assert isinstance(error, AttributeError)
    configs["geometry_overlay"]["schema_version"] = [3]
    error = _same_error(
        lambda: _read(world, "overlay_rooms"),
        lambda: configs["geometry_overlay"].get("schema_version") in {3, 4},
    )
    assert isinstance(error, TypeError)
    configs["simulator_extensions"] = ["enabled"]
    _same_error(
        lambda: _read(world, "extensions_enabled"),
        lambda: configs["simulator_extensions"].get("enabled"),
    )


def test_an_overlay_that_is_not_a_dict_has_no_room_schema_to_raise(world: WorldState) -> None:
    world.model_configs["geometry_overlay"] = [4]
    assert _read(world, "overlay_rooms") == (False, False)
    assert _read(world, "overlay_schema") is None


@pytest.mark.parametrize(
    ("edit", "text"),
    [
        ({"retry_delay_minutes": 2000}, "queue retry delay must be between 1 and 1440 minutes"),
        ({"schema_version": 2}, "facility_queues requires schema_version 1"),
        ({"other": 1}, "facility_queues config must be an object"),
        (
            {"abandon_after_minutes_by_service": {"meal": True}},
            "queue abandonment limits must be within 1..10080 minutes",
        ),
    ],
)
def test_the_queue_policy_raises_its_constructor_s_error_at_every_read(
    world: WorldState, edit: dict[str, Any], text: str
) -> None:
    world.model_configs["facility_queues"].update(edit)
    error = _same_error(
        lambda: _read(world, "facility_queues"),
        lambda: create_queue_policy(world.model_configs),
    )
    assert isinstance(error, ValueError)
    assert str(error) == text


def _refused(read: Any, text: str) -> None:
    for _ in range(2):
        with pytest.raises(sheltersim_native.Unsupported, match=f"^{text}$"):
            read()


def test_a_refusal_is_said_where_it_is_read_at_every_read(world: WorldState) -> None:
    configs = world.model_configs
    configs["movement_model"]["schema_version"] = 2
    # The section is read; its model is refused where a walk is timed.
    _refused(lambda: _read(world, "movement"), "another movement schema")
    route = ZoneRoute("Z1", "Z2", ("Z1", "Z2"), ("E1",), 3)
    agent_id = next(iter(world.agents))
    kernel = native.rules("movement").route_with_movement_time
    _refused(lambda: kernel(world, agent_id, route), "another movement schema")
    configs["facility_queues"]["retry_delay_minutes"] = 2**70
    _refused(lambda: _read(world, "facility_queues"), "an int is too large")
    configs["geometry_overlay"]["schema_version"] = 4.0
    _refused(lambda: _read(world, "overlay_schema"), "a value is not an int")
    configs["resource_service"] = [["water_staff_skill", "x"]]
    _refused(lambda: _read(world, "resource_service"), "the service config is another mapping")
    configs["population"]["people"]["A002"] = []
    _refused(lambda: _read(world, "accessibility_needs", "A002"), "a value is not a plain dict")
    configs["layout_routing"] = _routing({"e1": "x"})
    _refused(lambda: _read(world, "route_capacities"), "a value is not an int")
    configs["layout_routing"]["other"] = 1
    _refused(lambda: _read(world, "route_capacities"), "the route config is the Python rule's")


def test_a_fault_of_one_section_is_not_another_s(world: WorldState) -> None:
    configs = world.model_configs
    configs["movement_model"]["schema_version"] = 2
    configs["population"]["people"]["A002"] = []
    with pytest.raises(sheltersim_native.Unsupported):
        _read(world, "movement")
    with pytest.raises(sheltersim_native.Unsupported):
        _read(world, "accessibility_needs", "A002")
    assert _read(world, "accessibility_needs", "A001") == ["mobility_support"]
    assert _read(world, "facility_queues")[0] == "need_weighted"
    assert _read(world, "overlay_schema") == 4
    assert _read(world, "resource_service")[0] is True

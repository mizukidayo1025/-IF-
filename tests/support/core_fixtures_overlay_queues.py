"""The world of the independent queues' tests, for pytest and the core's tests (#225, lane M3).

`integration/test_overlay_rules.py` and `native/core/tests/overlay_rules.rs` check the rules of the
independent queues (`domain/facility_queues.py`), the toilet's and the water and meal service's
FIFOs and a decision to sleep in a room on the generated Scenario with two service points at two
facilities (`split_queues`, the `generated_scenario_with_split_queues` of `support/generated.py`)
and on the plain generated one (`support/core_fixtures_overlay.py`), both at minute 0 with the
simulator's extensions.

`overlay_queues_fixture` writes `native/core/tests/data/overlay_queues.json`: the split world, with
what the Run reads of a controller (`OfflineSimulationController`, made on a copy of the world so
that the world is the pytest's), and what the pytest read in Python: the order of the world's dicts,
the toilet manager's policy, and the water and meal service manager's config. The core's tests
change the access of a service point of the configs on the configs and on their section both:
`_access_is_its_section` checks the section of each such configs is that edit of the section.
"""

from __future__ import annotations

import copy
import json
from typing import Any

from support.core_fixtures_overlay import ordered, overlay_world

# The access of the service points the core's tests set (`the_shortest_accessible_queue`,
# `a_toilet_fifo_is_the_python_rule_s`).
ACCESS_EDITS = (
    {
        "accessibility": "restricted",
        "required_access_tags": ["key_holder"],
        "required_staff_skills": [],
    },
    {
        "accessibility": "restricted",
        "required_access_tags": ["mobility_support", "assisted"],
        "required_staff_skills": [],
    },
    {
        "accessibility": "restricted",
        "required_access_tags": ["assisted", "key_holder"],
        "required_staff_skills": [],
    },
    {
        "accessibility": "accessible",
        "required_access_tags": [],
        "required_staff_skills": ["no_such_skill"],
    },
)


def _access_is_its_section(world: Any, queue_id: str, access: dict[str, Any]) -> None:
    """The section of the configs with this access at the queue's point is the section with it."""
    from sheltersim.core.world_export import facility_geometry_section

    configs = copy.deepcopy(world.model_configs)
    section = copy.deepcopy(facility_geometry_section(configs)["value"])
    raw = configs["facility_geometry"]["service_points"]
    for point in (*raw, *section["service_points"]):
        if point["queue_id"] == queue_id:
            point["access"] = copy.deepcopy(access)
    assert facility_geometry_section(configs)["value"] == section


def _sink() -> Any:
    class _Sink:
        def __getattr__(self, name: str) -> Any:
            return lambda *arguments, **options: None

    return _Sink()


def overlay_queues_fixture() -> dict[str, Any]:
    """`overlay_queues.json`: the split world, with what the tests read in Python."""
    from sheltersim.core.world import WorldState
    from sheltersim.core.world_export import export_world
    from sheltersim.domain.facilities import ToiletServiceManager
    from sheltersim.domain.resource_service import ResourceServiceManager
    from sheltersim.experiments.runner import OfflineSimulationController
    from support.core_fixtures_queues import generated
    from support.generated import split_queues

    world = overlay_world(split_queues(generated()))
    for access in ACCESS_EDITS:
        _access_is_its_section(world, "toilet_01-left-fifo", access)
    controller = OfflineSimulationController(WorldState.from_dict(world.to_dict()), _sink())
    entry = export_world(world, controller)
    entry["order"] = {
        "zones": list(world.zones),
        "agents": list(world.agents),
        "facilities": list(world.facilities),
        "staff": list(world.staff),
    }
    entry["toilet_policy"] = ToiletServiceManager(world).policy_version
    entry["service_config"] = ordered(json.loads(json.dumps(ResourceServiceManager(world).config)))
    return {"worlds": {"split_queues": entry}}

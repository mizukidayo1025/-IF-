"""A checkpoint taken before independent queues existed keeps the old FIFO mode, view-only.

The queues themselves are run in tests/integration/test_facility_queues.py.
"""

from __future__ import annotations

from copy import deepcopy

import pytest

from sheltersim.core.checkpoint import Checkpoint
from sheltersim.core.model_version import ViewOnlyRunError
from sheltersim.core.world import WorldState


def test_prequeue_checkpoint_keeps_legacy_fifo_mode_and_is_only_viewed(
    generated_scenario_with_split_queues,
) -> None:
    from sheltersim.domain.facility_queues import (
        FACILITY_QUEUES_LEGACY_MODE_KEY,
        FACILITY_QUEUES_STATE_KEY,
    )

    world = WorldState.from_scenario(generated_scenario_with_split_queues)
    document = Checkpoint.capture(world).to_dict()
    legacy_world = deepcopy(document["world_state"])
    legacy_world["model_state"].pop(FACILITY_QUEUES_STATE_KEY)
    document["world_state"] = legacy_world
    document["model_versions"].pop("simulator:facility_queues_v1")
    document["state_hash"] = Checkpoint.document_hashes(document)[0]
    document["checkpoint_hash"] = Checkpoint.document_hashes(document)[1]

    # No tick reaches the old FIFO mode (core/model_version.py LEGACY_SHAPES_TICKABLE).
    with pytest.raises(ViewOnlyRunError):
        Checkpoint.from_dict(deepcopy(document))
    _, (viewed, _) = Checkpoint._load(deepcopy(document), view=True)
    assert viewed.model_state[FACILITY_QUEUES_LEGACY_MODE_KEY]["mode"] == (
        "pre_independent_queue_checkpoint"
    )
    assert FACILITY_QUEUES_STATE_KEY not in viewed.model_state
    with pytest.raises(ViewOnlyRunError):
        Checkpoint.capture(viewed)

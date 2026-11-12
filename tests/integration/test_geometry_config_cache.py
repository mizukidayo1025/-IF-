"""Tick phases reuse parsed facility geometry.

How the per-phase cache of the Python rules behaves is checked in
tests/unit/domain/test_geometry_overlay.py. The kernels parse the geometry once for a stamp of
model_configs, with or without that scope (tests/integration/test_geo_cache.py).
"""

from __future__ import annotations

from sheltersim.contracts.extensions import FacilityGeometry
from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
from sheltersim.domain.simulator_state import initialize_simulator_extensions
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink


def test_tick_phases_reuse_parsed_geometry(generated_scenario, count_validations):
    """The kernels parse the configs once for each stamp of model_configs, not at each tick."""
    world = WorldState.from_scenario(generated_scenario)
    initialize_simulator_extensions(world)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    calls = count_validations(FacilityGeometry)
    for _ in range(10):
        engine.step()
    assert len(calls) < 10

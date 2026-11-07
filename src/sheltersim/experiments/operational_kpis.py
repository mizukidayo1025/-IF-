"""Minute-level observational KPIs for opt-in water and meal operations."""

from __future__ import annotations

from typing import Any

from sheltersim.core.kernels import kernel_only
from sheltersim.core.world import WorldState
from sheltersim.experiments.metrics import MetricSnapshot

OPERATIONAL_KPI_FIELDS = (
    "minute_index",
    "toilet_urgency_mean",
    "hunger_need_mean",
    "hydration_need_mean",
    "water_queue_length",
    "meal_queue_length",
    "water_active_service_count",
    "meal_active_service_count",
    "water_service_wait_count",
    "water_service_wait_total_minutes",
    "water_service_wait_mean_minutes",
    "water_service_wait_max_minutes",
    "meal_service_wait_count",
    "meal_service_wait_total_minutes",
    "meal_service_wait_mean_minutes",
    "meal_service_wait_max_minutes",
    "water_outage_stall_count",
    "resource_stock_stall_count",
    "water_ml_remaining",
    "food_meal_units_remaining",
    "water_ml_reserved",
    "food_meal_units_reserved",
    "stress_mean",
    "staff_busy_count",
    "staff_load_ratio",
    "unresolved_support_request_count",
    "support_no_qualified_worker_count",
    "support_no_qualified_worker_request_minutes",
    "support_fairness_promoted_assignment_count",
    "support_wait_staff_busy_request_minutes",
    "support_wait_staff_off_shift_request_minutes",
)


@kernel_only("metrics", "kpi_values")
def operational_kpi_values(world: WorldState, snapshot: MetricSnapshot) -> dict[str, Any]:
    """Return detached aggregate measurements without changing simulation state.

    The columns of OPERATIONAL_KPI_FIELDS (in the order: the minute, the three need means over all
    Agents, the queues and services of the water stations and meal distributions, remaining and
    reserved stock, the snapshot's staff and support columns, then the water and meal service
    waits, whose means are 0.0 with no wait, and the outage and stock stall counts from
    `world.metric_state`). ValueError for a world that is not v6 with resource_service_v1, or a
    snapshot of another minute; ZeroDivisionError with no Agents.
    """

"""Pure per-minute KPI snapshots for reproducible run inspection."""

from __future__ import annotations

from dataclasses import asdict, dataclass
from typing import Any

from sheltersim.core.kernels import kernel_only
from sheltersim.core.world import WorldState
from sheltersim.domain.resources import RESOURCE_METRIC_FIELDS, resource_metric_values


@kernel_only("staff", "wait_snapshot")
def _support_wait_snapshot(
    world: WorldState, active_support: list[Any], sample_minute: int
) -> tuple[dict[str, dict[str, Any]], list[str]]:
    """The wait of each open support request, and who waits with no qualified staff at all.

    The first is by agent_id, in agent_id order (a later request of the same agent replaces an
    earlier one): its request_id, `support_waited_minutes` and `support_wait_reason_codes` at the
    sample minute. The second is the sorted agent_ids of the pending requests whose required skills
    no staff member has.
    """


@kernel_only("metrics", "snapshot_fields")
def _snapshot_fields(
    world: WorldState, unsettled: list[Any], sample_minute: int
) -> tuple[Any, ...]:
    """The fields of the minute's MetricSnapshot, in the order of the dataclass.

    The toilets are the facilities whose kind has "toilet" in it, casefolded. The means are 0.0
    with nothing to average. `source_ids_json` is the canonical JSON of the sorted IDs behind the
    counts: queued and served toilet users, people with stress of at least 0.7, staff tasks, busy
    staff, people with an open (pending or assigned) support request and those of them without any
    qualified staff, the wait of each (`_support_wait_snapshot`), and the events realized at the
    minute. The last fields are read from `world.metric_state`, 0.0 when absent.
    """


@dataclass(frozen=True)
class MetricSnapshot:
    """Aggregated metrics captured without mutating simulation state."""

    minute_index: int
    toilet_queue_length: int
    toilet_active_users: int
    toilet_working_units: int
    toilet_wait_count: float
    toilet_wait_total_minutes: float
    toilet_wait_mean_minutes: float
    toilet_wait_max_minutes: float
    stress_mean: float
    stress_high_count: int
    staff_assigned_task_count: int
    staff_busy_count: int
    staff_load_ratio: float
    unresolved_support_request_count: int
    source_ids_json: str
    support_no_qualified_worker_count: int = 0
    support_no_qualified_worker_request_minutes: float = 0.0
    support_fairness_promoted_assignment_count: float = 0.0
    support_wait_staff_busy_request_minutes: float = 0.0
    support_wait_staff_off_shift_request_minutes: float = 0.0

    @classmethod
    def from_world(cls, world: WorldState, *, minute: int | None = None) -> MetricSnapshot:
        """Measure queue, wait, stress, staff load and pending help from current state."""
        sample_minute = world.minute_index if minute is None else minute
        if sample_minute != world.minute_index:
            raise ValueError("metric minute must match WorldState")
        return cls(
            *_snapshot_fields(world, list(world.unsettled_support_requests()), sample_minute)
        )

    def csv_row(self) -> dict[str, Any]:
        """Return a detached row with stable CSV column names."""
        return asdict(self)


class MetricsRecorder:
    """Collect one snapshot at each explicit collect-metrics phase."""

    def __init__(self) -> None:
        self.snapshots: list[MetricSnapshot] = []
        self.resource_snapshots: list[dict[str, int]] | None = None

    def capture(self, world: WorldState, *, minute: int | None = None) -> MetricSnapshot:
        snapshot = MetricSnapshot.from_world(world, minute=minute)
        if self.snapshots and snapshot.minute_index <= self.snapshots[-1].minute_index:
            raise ValueError("metric snapshots must have strictly increasing minutes")
        resource_values = resource_metric_values(world)
        if resource_values is None:
            if self.resource_snapshots is not None:
                raise ValueError("resource ledger cannot be removed during metric capture")
        else:
            if self.resource_snapshots is None:
                if self.snapshots:
                    raise ValueError("resource ledger must be enabled before metric capture")
                self.resource_snapshots = []
            self.resource_snapshots.append(
                {"minute_index": snapshot.minute_index, **resource_values}
            )
        self.snapshots.append(snapshot)
        return snapshot

    def to_dict(self) -> dict[str, Any]:
        """Serialize the complete metric accumulator for checkpoint continuation."""
        return self._document(
            [asdict(item) for item in self.snapshots],
            None
            if self.resource_snapshots is None
            else [dict(item) for item in self.resource_snapshots],
        )

    @staticmethod
    def _document(
        snapshots: list[dict[str, Any]], resource_snapshots: list[dict[str, int]] | None
    ) -> dict[str, Any]:
        """to_dict() of sample documents a Checkpoint save may have converted before."""
        document: dict[str, Any] = {"schema_version": 4, "snapshots": snapshots}
        if resource_snapshots is not None:
            document["resource_snapshots"] = resource_snapshots
        return document

    @classmethod
    def from_dict(
        cls,
        document: dict[str, Any],
        *,
        _leading: tuple[list[MetricSnapshot], list[dict[str, int]]] = ([], []),
    ) -> MetricsRecorder:
        """Restore legacy KPI snapshots and optional resource samples.

        _leading holds what an earlier restore made of the leading samples of this same
        document (a Checkpoint capture's restore check): they are shared, and only the
        samples after them are read and checked.
        """
        if (
            not isinstance(document, dict)
            or not isinstance(document.get("schema_version"), int)
            or isinstance(document.get("schema_version"), bool)
            or document.get("schema_version") not in (1, 2, 3, 4)
        ):
            raise ValueError("unsupported metric accumulator schema")
        version = document["schema_version"]
        expected_fields = {"schema_version", "snapshots"}
        allowed_fields = (
            {frozenset(expected_fields)}
            if version == 1
            else {
                frozenset(expected_fields),
                frozenset(expected_fields | {"resource_snapshots"}),
            }
        )
        if frozenset(document) not in allowed_fields:
            raise ValueError(f"metric accumulator fields do not match schema v{version}")
        snapshots = document.get("snapshots")
        if not isinstance(snapshots, list):
            raise ValueError("metric accumulator snapshots must be a list")
        recorder = cls()
        try:
            restored_snapshots = list(_leading[0])
            for item in snapshots[len(_leading[0]) :]:
                row = dict(item)
                if version < 3:
                    row.setdefault("support_no_qualified_worker_count", 0)
                    row.setdefault("support_no_qualified_worker_request_minutes", 0.0)
                    row.setdefault("support_fairness_promoted_assignment_count", 0.0)
                if version < 4:
                    row.setdefault("support_wait_staff_busy_request_minutes", 0.0)
                    row.setdefault("support_wait_staff_off_shift_request_minutes", 0.0)
                restored_snapshots.append(MetricSnapshot(**row))
            recorder.snapshots = restored_snapshots
        except (TypeError, ValueError) as exc:
            raise ValueError(f"invalid metric accumulator: {exc}") from exc
        # The leading samples were checked; from the last of them on the rest are.
        checked = max(len(_leading[0]) - 1, 0)
        minutes = [item.minute_index for item in recorder.snapshots[checked:]]
        if any(minute < 0 for minute in minutes) or minutes != sorted(set(minutes)):
            raise ValueError("metric accumulator minutes must be unique and increasing")
        if version == 1 and "resource_snapshots" in document:
            raise ValueError("metric accumulator v1 cannot contain resource snapshots")
        if "resource_snapshots" in document:
            resource_snapshots = document["resource_snapshots"]
            if not isinstance(resource_snapshots, list) or len(resource_snapshots) != len(
                snapshots
            ):
                raise ValueError("resource metric snapshots must align with metric snapshots")
            expected_resource_fields = {"minute_index", *RESOURCE_METRIC_FIELDS}
            leading = len(_leading[1])
            for item, sample in zip(
                resource_snapshots[leading:], recorder.snapshots[leading:], strict=True
            ):
                expected_minute = sample.minute_index
                if not isinstance(item, dict) or set(item) != expected_resource_fields:
                    raise ValueError("resource metric snapshot fields do not match schema v1")
                if item["minute_index"] != expected_minute or any(
                    not isinstance(value, int) or isinstance(value, bool) or value < 0
                    for key, value in item.items()
                    if key != "minute_index"
                ):
                    raise ValueError("resource metric snapshot values are invalid")
            recorder.resource_snapshots = [
                *_leading[1],
                *(dict(item) for item in resource_snapshots[leading:]),
            ]
        return recorder

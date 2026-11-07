"""Compare `apply_effect` of native/src/rules/proposals.rs with `_apply_effect` at each call.

The kernel runs on the world first, and the Python step then runs on a copy made before it: plain
copies of the people, the bed reservations and the event counter, and a list of what it schedules.
The result, what those hold and each event's repr must come out the same. What the kernel refuses
isn't compared: the Python step runs on the world then. A trip, and sleep where a room may have a
bed, are not compared: their Python step goes through `_route` and `_schedule_travel`, which have
no Python step left (lane L10).
"""

from __future__ import annotations

from types import SimpleNamespace
from typing import Any

from shadow_handlers import seek_shadow
from shadow_kit import Recorder, text, values_in

TRIPS = {"request_toilet", "fetch_water", "collect_meal"}
SEEKS = {"seek_help", "seek_information"}
OVERLAY = "geometry_overlay_v1"
ROW_CHECK_EVERY = 500


class _Twin:
    def __init__(self, world: Any) -> None:
        table = dict(world.model_state)
        overlay = table.get(OVERLAY)
        if isinstance(overlay, dict) and isinstance(overlay.get("bed_reservations"), dict):
            table[OVERLAY] = {**overlay, "bed_reservations": dict(overlay["bed_reservations"])}
        self.__dict__.update(
            _world=world,
            model_state=table,
            id_counters=dict(world.id_counters),
            agents={
                key: SimpleNamespace(**vars(item), id=item.id) for key, item in world.agents.items()
            },
            scheduled=[],
        )

    def __getattr__(self, name: str) -> Any:
        return getattr(self.__dict__["_world"], name)

    def allocate_event_sequence(self) -> int:
        counters = self.__dict__["id_counters"]
        sequence = counters.get("event", 0)
        counters["event"] = sequence + 1
        return sequence

    def schedule_event(self, event: Any) -> None:
        self.__dict__["scheduled"].append(event)
        counters = self.__dict__["id_counters"]
        counters["event"] = max(counters.get("event", 0), event.sequence + 1)


def prepare(output: str) -> None:
    from sheltersim import native

    rules = native.rules("proposals")
    recorder = Recorder(output, "proposals")
    unsupported = native.Unsupported
    kernel = rules.apply_effect
    # shadow_handlers.py's comparison: it sorts before this file, so it has prepared.
    seek = seek_shadow(kernel)

    def trip(controller: Any, proposal: Any, action: str) -> Any:
        """The kernel alone: a trip's Python step goes through `_route` and `_schedule_travel`,
        which have no Python step left to run on a copy (KERNEL_ONLY_SEGMENTS, lane L10)."""
        try:
            result = kernel(controller, proposal)
        except unsupported as reason:
            recorder.reasons[f"{action}: {reason}"] += 1
            raise
        recorder.seen["kernel_ran"] += 1
        recorder.seen[f"action.{action}"] += 1
        recorder.seen[f"action.{action}.{'moves' if result[0] else 'stays'}"] += 1
        return result

    def apply_effect(controller: Any, proposal: Any) -> Any:
        from sheltersim.experiments.phases import propose

        world = controller.world
        action = proposal.action.value
        recorder.seen["calls"] += 1
        overlay = world.model_state.get(OVERLAY)
        walks_to_bed = (
            action == "sleep" and isinstance(overlay, dict) and bool(overlay.get("temporary_rooms"))
        )
        if action in TRIPS or walks_to_bed:
            return trip(controller, proposal, action)
        if action in SEEKS:
            recorder.seen[f"action.{action}"] += 1
            return seek(controller, proposal)
        twin = _Twin(world)
        pending_before = len(world.event_queue.pending())
        try:
            result = kernel(controller, proposal)
        except unsupported as reason:
            recorder.reasons[f"{action}: {reason}"] += 1
            raise
        recorder.seen["kernel_ran"] += 1
        recorder.seen[f"action.{action}"] += 1
        where = f"{action} of {proposal.agent_id} at {world.minute_index}"
        stand_in = SimpleNamespace(world=twin)
        # The kernel of the capture reads the world's rows, not the copy's: the proposal changed
        # none of the values it copies.
        capture = propose.capture_social_state_context
        propose.capture_social_state_context = lambda _twin, ids: capture(world, ids)
        try:
            expected = propose.ProposePhase._apply_effect(stand_in, proposal)
        except Exception as raised:  # noqa: BLE001 - only compared with what the kernel did
            recorder.differ(f"{where}: the kernel took what Python turned down: {raised!r:.300}")
            return result
        finally:
            propose.capture_social_state_context = capture
        if text(list(result)) != text(list(expected)):
            recorder.differ(f"{where}: returned {result!r:.100}, not {expected!r:.100}")
        fields = twin.__dict__
        if world.id_counters != fields["id_counters"]:
            recorder.differ(f"{where}: the counters")
        for key, agent in world.agents.items():
            theirs = fields["agents"][key]
            if (
                agent.activity is not theirs.activity
                or agent.target_id != theirs.target_id
                or agent.next_decision_at != theirs.next_decision_at
            ):
                recorder.differ(f"{where}: {key}")
        overlay = world.model_state.get(OVERLAY)
        if isinstance(overlay, dict):
            mine = text(overlay.get("bed_reservations"))
            if mine != text(fields["model_state"][OVERLAY].get("bed_reservations")):
                recorder.differ(f"{where}: the bed reservations")
            recorder.seen["beds_compared"] += 1
        pending = {event.event_id: event for event in world.event_queue.pending()}
        if len(pending) != pending_before + len(fields["scheduled"]):
            recorder.differ(f"{where}: {len(pending)} pending, {pending_before} before")
        for event in fields["scheduled"]:
            recorder.seen["events_compared"] += 1
            recorder.seen["values_compared"] += values_in(event.to_dict())
            real = pending.get(event.event_id)
            if real is None or repr(real) != repr(event):
                recorder.differ(f"{where}: event {real!r:.400} is not {event!r:.400}")
        recorder.seen["values_compared"] += 6
        if recorder.seen["kernel_ran"] % ROW_CHECK_EVERY == 0:
            recorder.seen["rows_verified"] += 1
            problem = world._core.entities().verify(world)
            if problem:
                recorder.differ(f"{where}: the rows and the objects: {problem}")
        return result

    rules.apply_effect = apply_effect

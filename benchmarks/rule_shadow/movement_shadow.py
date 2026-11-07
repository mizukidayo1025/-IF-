"""Compare the row of movement_v1 with its tree at every tick.

sitecustomize.py installs this with the others; `summary.py DIR` adds up what it saw. The movement
rules are still Python (docs/rust-port/lanes/i-movement.md), so what moved is the state: after the
movement step and at the end of each tick, the row must write the bytes of the tree, and it must
not be tainted. The tree's values were compared with plain dicts by `first_difference.py --both`,
which ran without the Rust extension; that mode went when the extension became required.

`seen` has how many values were compared, how many writes were forwarded, and which branches were
reached, since a branch no scenario takes is not compared.

Reversing `advance_movement`'s order (SHELTERSIM_MOVEMENT_REVERSED) went with its Python rule (lane
L10): the kernel plans from what the tick started with, so it has no order of people.
"""

from __future__ import annotations

from collections import Counter
from typing import Any


def _leaves(value: Any) -> int:
    if isinstance(value, dict):
        return sum(_leaves(item) for item in value.values())
    if isinstance(value, list):
        return sum(_leaves(item) for item in value)
    return 1


def install(seen: Counter[str], reasons: Counter[str], differences: list[str]) -> None:
    from sheltersim import native
    from sheltersim.core.engine import TickEngine
    from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
    from sheltersim.experiments.phases import update
    from sheltersim.experiments.phases.update import UpdatePhase

    # The one kernel that reads this tree. Refusing it costs time and changes no hash.
    positions_valid = native.movement_positions_valid

    def counted_positions_valid(*arguments: Any) -> bool:
        accepted = positions_valid(*arguments)
        seen[
            "movement_positions_kernel_accepted"
            if accepted
            else "movement_positions_kernel_refused"
        ] += 1
        return accepted

    native.movement_positions_valid = counted_positions_valid
    forwards: dict[int, int] = {}
    whole_writes: dict[int, int] = {}
    journeys: dict[int, set[str]] = {}

    def compare(world: Any, where: str) -> None:
        core = world.__dict__.get("_core")
        if core is None or KEY not in world.model_state or KEY not in core.owned_names():
            return
        mirror = core.mirror(KEY)
        seen[f"movement_compares_{where}"] += 1
        seen["movement_worlds_seen"] += id(mirror) not in forwards
        tainted = mirror.tainted
        if tainted is not None:
            reasons[f"movement_v1 tainted: {tainted}"] += 1
            return
        if not mirror.verify():
            differences.append(f"minute {world.minute_index}, {where}: the row and tree of {KEY}")
        tree = world.model_state[KEY]
        seen["movement_values_compared"] += _leaves(tree)
        stats = mirror.stats()
        seen["movement_forwards"] += stats["forwards"] - forwards.get(id(mirror), 0)
        forwards[id(mirror)] = stats["forwards"]
        seen["movement_whole_writes"] += stats["whole_writes"] - whole_writes.get(id(mirror), 0)
        whole_writes[id(mirror)] = stats["whole_writes"]
        known = journeys.setdefault(id(world), set())
        current = {journey["event_id"] for journey in tree["journeys"].values()}
        seen["movement_journeys_new"] += len(current - known)
        known |= current
        for journey in tree["journeys"].values():
            seen["movement_journeys_with_room_points"] += bool(journey["route_points"])
            seen["movement_journeys_with_queue_id"] += "queue_id" in journey
            seen["movement_journeys_with_support"] += "support_request_id" in journey

    python_step = UpdatePhase._update_movement

    def update_movement(self: Any, minute: int) -> None:
        python_step(self, minute)
        compare(self.world, "after_movement")

    UpdatePhase._update_movement = update_movement  # type: ignore[method-assign]
    python_tick = TickEngine.step

    def step(self: Any) -> Any:
        result = python_tick(self)
        compare(self.world, "after_tick")
        return result

    TickEngine.step = step  # type: ignore[method-assign]
    advance = update.advance_movement

    def counted_advance(world: Any, minute: int) -> tuple[str, ...]:
        replans = advance(world, minute)
        seen["movement_advances"] += 1
        seen["movement_replans"] += len(replans)
        seen["movement_ticks_with_two_replans"] += len(replans) > 1
        return replans

    update.advance_movement = counted_advance

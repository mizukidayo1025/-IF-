"""The propose phase: each due decision, and the trips and requests it starts.

The other phases plan and schedule trips with the route and travel methods here too.
"""

from __future__ import annotations

import json
from typing import Any

from sheltersim.core.rng import KeyedRandom
from sheltersim.core.world import WorldState
from sheltersim.domain.movement import (
    facility_approach_route,
)
from sheltersim.llm.contracts import LLMStage, LLMTask
from sheltersim.llm.runtime import LEGACY_SUPPORT_PRIORITY_POLICY_VERSION, support_priority_runtime
from sheltersim.llm.schemas import SUPPORT_ADVICE_SCHEMA
from sheltersim.policies.space import ZoneRoute
from sheltersim.policies.world_view import WorldView


class ProposePhase:
    """Steps of TickPhase.PROPOSE_ACTIONS, a mixin of OfflineSimulationController."""

    def _run_propose_phase(self, world: WorldState, minute: int, rng: KeyedRandom) -> None:
        view = self._view_for_proposals(world, minute)
        self._apply_due_proposals(view, rng)

    def _view_for_proposals(self, world: WorldState, minute: int) -> WorldView:
        """The view the decisions read, which native/src/rules/world_view.rs makes.

        When `_view_before_proposals` is the update phase's view of `minute`, that view with what
        the update phase writes read again (each person's needs, stress, trust, body state and
        long-term distress); otherwise a new view of `world`. Either has the audit hash of what a
        decision observed (the SHA-256 of the view's canonical document, native/src/records.rs).
        The step empties `_view_before_proposals`.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: a prior view no kernel made, a person of the prior view who has left,
        and what `_view_after_movement` turns down.
        """

    def _apply_due_proposals(self, view: WorldView, rng: KeyedRandom) -> None:
        for proposal in self._propose_due(view, rng):
            self._apply_proposal(
                proposal,
                observed_state=self._observed_state(view, proposal.agent_id),
            )

    def _propose_due(self, view: WorldView, rng: KeyedRandom) -> tuple[Any, ...]:
        """The proposals of everyone due, made by native/core/src/rules/behavior.rs.

        In ID order, one for each person whose `next_decision_at` has come and who is idle,
        resting or sleeping. `decisions.policy` (`UtilityBehaviorPolicy`, or a
        `PriorityRuleBehaviorPolicy` around one) scores the candidate actions (`ActionKind`) and
        draws the choice from `rng`, keyed by the person, the minute and the policy version. Each
        is a `ProposalRow` that reads as the `ActionProposal` it stands for (`materialize()` makes
        that). Nothing changes.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: a runner, policy or scorer of another class, an RNG other than
        CounterRNG or with a negative seed, a view no kernel made or without its audit hash, and
        values on which `ActionCandidate` or `ActionProposal` would raise (a bad temperature or
        decision interval).
        """

    def _observed_state(self, view: WorldView, agent_id: str) -> Any:
        """What a decision records it observed of the person (native/core/src/rules/behavior.rs).

        The person's ID, zone, activity and target, needs (with `hydration_need` when the resource
        service is on), psychological values, the zone's sanitation, stress, trust, queue and
        support request, and the zone's environment when it has modifiers. An `ObservedRow` that
        `dict()` makes the document of; a dict itself for a sink that reads it in Python.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); a view no kernel made
        or a person not in it raises KernelRefusal.
        """

    def _apply_proposal(
        self,
        proposal: Any,
        *,
        observed_state: dict[str, Any] | None = None,
    ) -> None:
        target_id, next_decision_at, committed = self._apply_effect(proposal)
        self._record_outcome(
            proposal,
            target_id=target_id,
            next_decision_at=next_decision_at,
            committed=committed,
            observed_state=observed_state,
        )

    def _apply_effect(self, proposal: Any) -> tuple[str | None, int, bool]:
        """What the decision does to the world, which native/src/rules/proposals.rs does.

        The person's `next_decision_at` becomes the proposal's, and a bed they hold is given up
        unless they sleep. `wait` and `rest` make them idle or resting. `sleep` reserves a bed: they
        walk to it (a `sleep_bed` trip) or lie down there, or sleep where they are without one.
        `seek_help` and `seek_information` ask `staff_service` for support, with the care receipts
        of a `seek_help`, the information model's record, the Provider's priority
        (`_prioritize_support_request`) and `social_policy.consider_support_request` for a pending
        request; a refused request adds to `support_request_rejected_count`. `socialize`,
        `care_family` and `help_others` schedule a `social_interaction` event with a co-located
        participant, which holds the next decision until it. `request_toilet`, `fetch_water` and
        `collect_meal` choose the known facility with the least load, then the shortest trip, then
        the least ID, and queue there at once or start the trip; the resource service records each
        outcome and counts rejections in `resource_service_request_rejected_count`.

        Returns the target (participant, request, bed or facility), the next decision minute, and
        whether the decision committed: a target, an activity of its own, or a changed activity,
        but not a request closed at once.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: an action it does not know, a person not in the world, state of
        another type than the rows hold, a service manager whose `request` was replaced.
        """

    def _record_outcome(
        self,
        proposal: Any,
        *,
        target_id: str | None,
        next_decision_at: int,
        committed: bool,
        observed_state: Any,
    ) -> None:
        """Record the decision with its outcome, which native/core/src/rules/decisions.rs does.

        In this order: `artifacts.record_decision(proposal.with_outcome(target_id,
        next_decision_at), observed_state=observed_state)`, which the kernel writes itself for a
        sink that declares `decision_sink` (the same record, or a trace's document); the action's
        lifecycle (`record_action_proposal` with `committed` and `active_actions`); and, when
        `agent_dynamics_v1` exists, the person's intention (`record_proposal`). A proposal
        `with_outcome` refuses (a next decision not after its minute) raises its ValueError before
        anything is recorded.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what the rules it
        calls turn down raises KernelRefusal.
        """

    def _prioritize_support_request(self, request_id: str, proposal: Any) -> None:
        if self.llm_router is None or self.llm_router.mode.value == "offline":
            return
        agent = self.world.agents[proposal.agent_id]
        constraints = [
            "Use only the functional need and current state values below.",
            "Do not infer from names, demographics, or group membership.",
            "Do not approve or deny support; this only orders a staff queue.",
            "Return the required JSON object only.",
        ]
        if self.support_priority_version != LEGACY_SUPPORT_PRIORITY_POLICY_VERSION:
            # v2 asks only for stressed requesters within a daily cap; skipped
            # requests keep the rule priority 0, exactly like a fallback.
            runtime = support_priority_runtime(self.world)
            day_key = str(self.world.minute_index // 1440)
            skip_reason = (
                "below_stress_threshold"
                if agent.stress < self.support_priority_limits.stress_threshold
                else "global_daily_cap"
                if runtime["attempts_by_day"].get(day_key, 0)
                >= self.support_priority_limits.global_per_day
                else None
            )
            if skip_reason is not None:
                skipped = runtime["skipped_by_reason"]
                skipped[skip_reason] = skipped.get(skip_reason, 0) + 1
                return
            attempts = runtime["attempts_by_day"]
            attempts[day_key] = attempts.get(day_key, 0) + 1
            constraints.append("Keep rationale to one short phrase of at most 80 characters.")
        prompt = {
            "task": "Set queue priority for one synthetic staff support request.",
            "allowed_priority": {"0": "normal", "1": "urgent"},
            "constraints": constraints,
            "minute": self.world.minute_index,
            "support_need": agent.profile.support_need,
            "stress": agent.stress,
            "trust": agent.trust,
            "needs": {
                "toilet_urgency": agent.needs.toilet_urgency,
                "hunger": agent.needs.hunger,
                "fatigue": agent.needs.fatigue,
            },
        }
        input_text = json.dumps(
            prompt,
            sort_keys=True,
            ensure_ascii=False,
            separators=(",", ":"),
            allow_nan=False,
        )
        response = self.llm_router.complete(
            LLMTask(
                stage=LLMStage.RUNTIME,
                input_text=input_text,
                state_hash=proposal.observed_state_hash,
                prompt_version=self.support_priority_version,
                schema_version=1,
                output_schema=SUPPORT_ADVICE_SCHEMA,
                fallback_text='{"priority":0,"rationale":"rule-based fallback"}',
                max_output_tokens=96,
            )
        )
        result = json.loads(response)
        self.world.model_state.setdefault("llm", {}).setdefault("support_priorities", {})[
            request_id
        ] = result["priority"]

    def _schedule_travel(
        self,
        *,
        agent_id: str,
        route: ZoneRoute,
        route_points: tuple[Any, ...] = (),
        purpose: str,
        facility_id: str | None,
        minute: int,
        causation_id: str | None = None,
        support_request_id: str | None = None,
        support_staff_id: str | None = None,
        validate_state: bool = True,
    ) -> bool:
        """Start one trip along `route`, which native/src/rules/travel.rs plans and writes.

        ValueError("travel route origin does not match Agent zone") when the route does not start
        in the person's zone. False, with nothing changed, for a route of no minutes, one that
        ends at or past the Run's end, or one into another zone whose capacity its occupants and
        those walking to it already fill. Otherwise True: an `agent_travel_complete` event
        `agent-travel-complete-<sequence>` due at `minute + route.travel_minutes` (the payload
        with the route, `origin_zone_id` for a resource trip, `request_id` and `staff_id` for a
        support trip; the source of `layout_routing` or ASM-ZONE-ROUTING-V1), its task
        `agent-travel-<sequence>` (travel_to_toilet, travel_to_resource, staff_support_travel or
        travel_return), the person MOVING to the facility of a toilet or resource trip or else
        the destination zone, and with `movement_v1` the journey and its `route_points`. A
        `staff_support_travel` trip without both IDs raises ValueError("staff support travel
        requires request and staff IDs"). With `validate_state`, `world.validate()` after.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal, with nothing changed: an unknown person or zone, a toilet or
        resource trip without a facility, a sequence whose IDs are taken.
        """

    def _schedule_staff_support_travel(
        self,
        agent_id: str,
        route: ZoneRoute,
        request_id: str,
        staff_id: str,
        minute: int,
    ) -> bool:
        """`_schedule_travel` of a `staff_support_travel` trip caused by the request, without
        validating the whole state: native/src/rules/travel.rs. StaffService's
        `travel_scheduler`.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS).
        """

    def _route(
        self, origin_zone_id: str, destination_zone_id: str, agent_id: str
    ) -> ZoneRoute | None:
        """The shortest route for this person, or None: native/src/rules/travel.rs.

        Over the graph the last `_refresh_space` built from the world's zone and edge tables (the
        same as `space`'s) without the edges a floor plan closes to the person's mobility.
        With `route_capacities`, an edge whose trips under way (pending `agent_travel_complete`
        events not yet due, staff operation trips) reach its capacity is avoided; when that
        leaves no route but a free graph has one, the first full edge of it is recorded as a
        SUPPRESSED `zone_edge_capacity_wait` event ("route edge capacity is full; the Agent
        remains at its origin"). The route's minutes are those of `route_with_movement_time`.
        StaffService's `route_resolver`.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: an unknown person or zone, a route policy other than
        ShortestTravelTimePolicy (read only where the kernel reads `space`: zone rows that do not
        follow the world, or a refresh of the Run's routes without a world).
        """

    def _facility_approach(
        self, agent_id: str, facility_id: str
    ) -> tuple[ZoneRoute, tuple[Any, ...]] | None:
        """Plan the wall-aware, mobility-specific room path to the service point."""
        return facility_approach_route(self.world, agent_id, facility_id)

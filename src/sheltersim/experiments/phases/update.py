"""The update phase: movement, then each person's psychology and social propagation."""

from __future__ import annotations

from typing import Any

from sheltersim.core.rng import KeyedRandom
from sheltersim.core.world import WorldState
from sheltersim.domain.movement import MOVEMENT_STATE_KEY, advance_movement
from sheltersim.policies.psychology import PsychologyUpdateResult
from sheltersim.policies.world_view import WorldView


class UpdatePhase:
    """Steps of TickPhase.UPDATE_AGENT_STATE, a mixin of OfflineSimulationController."""

    @property
    def _view_before_proposals(self) -> WorldView | None:
        """The update phase's view of this tick, which the propose phase builds on.

        The Run's core keeps it (`Run.scratch`, native/core/src/rules/tick): the update phase's
        snapshot, made a `WorldView` at each read, or a view set here, which is given back as it
        was set. None before the update phase and after the propose phase.
        """
        run = self.__dict__.get("_run_core")
        if run is None:
            return None
        return run.view_before_proposals()

    @_view_before_proposals.setter
    def _view_before_proposals(self, view: WorldView | None) -> None:
        run = self.__dict__.get("_run_core")
        if run is None:
            # The controller's `__init__` empties it before its Run's core exists.
            if view is not None:
                raise RuntimeError("the controller has no Run core to keep a view")
            return
        run.set_view_before_proposals(view)

    def _run_update_phase(self, world: WorldState, minute: int, rng: KeyedRandom) -> None:
        self._update_movement(minute)
        view = self._view_after_movement(world)
        psychology_result = self._psychology_update(view, minute)
        final_deltas = self._social_propagation(world, view, psychology_result.deltas, minute, rng)
        self._record_psychology_trace(psychology_result, final_deltas, minute)
        self._update_long_term_psychology(world, view, final_deltas, minute)
        self._apply_state_deltas(world, final_deltas)
        self._view_before_proposals = view

    def _view_after_movement(self, world: WorldState) -> WorldView:
        """The view the update phase reads, which native/src/rules/world_view.rs makes.

        A `WorldView` of `world` at its minute, without an audit hash (`state_hash` None), read
        from the rows and the namespaces. Its `agents`, `facilities`, `zone_members` and
        `group_members` are made when a Python reader first asks for them. Parsed person contexts
        and zone conditions are kept in the controller's Run (`_run_core`) while the stamps of what
        they were read from stay the same.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: a value of another type than its typed field holds, a person in an
        unknown zone, a person context or a zone modifier that does not parse, a selection of
        models the contract refuses.
        """

    def _psychology_update(self, view: WorldView, minute: int) -> PsychologyUpdateResult:
        """Each person's values of the next minute, which native/core/src/rules/psychology.rs
        computes.

        The model is `psychology` (the five models of policies/psychology.py and
        BoundedStressPolicy of policies/alternatives.py, by their exact class). The result holds
        one new AgentStateDelta per person of `view`, in ID order, and `policy_version`; its
        `traces` are the kernel's rows of each person's contributions when the Run keeps a trace,
        and () otherwise. Nothing changes: the deltas are written by `_apply_state_deltas`.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: a model of another class, a minute other than the view's, a trace
        on a view Python made, an entity store that a write past its rows tainted, a trust, a
        sleep debt, a sleep efficiency or a required sleep out of bounds, or a new value outside
        an AgentStateDelta's bounds.
        """

    def _social_propagation(
        self, world: WorldState, view: WorldView, base_deltas: Any, minute: int, rng: KeyedRandom
    ) -> Any:
        """The deltas after the social model, which is native/src/rules/social_propagation.rs.

        With `stress_policy.parameters.enabled` false, `base_deltas` themselves. Otherwise the
        model's contacts of this minute (drawn from `rng` per person, from household, zone and
        `zone_neighbors`) move each person's stress, and the step writes the new contacts and
        cooldowns to `world.model_state["social_propagation_v1"]`, records each contact in
        `social_network_v1` when that namespace exists, and gives the contact trace to
        `artifacts.record_social_contact_trace` when the Run keeps a trace. The deltas are new
        AgentStateDelta objects, in ID order.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: a namespace of another schema or with negative minutes, deltas or a
        view of another minute, an RNG other than CounterRNG, a policy class it does not know.
        """

    def _record_psychology_trace(
        self, psychology_result: PsychologyUpdateResult, final_deltas: Any, minute: int
    ) -> None:
        """The psychology trace of the minute, which native/core/src/rules/trace.rs writes.

        Nothing when the Run keeps no trace. Otherwise `artifacts.record_psychology_trace` gets
        the minute's document and `agent_contributions`, one per person: each person's stress
        after is the final delta's, so `social_stress_delta` is what the social step added. A full
        trace keeps every person, a summary the 20 whose stress changed most (then by ID); the
        component totals are over everyone.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: traces that are not the psychology kernel's rows, or a person
        without a final delta.
        """

    def _update_long_term_psychology(
        self, world: WorldState, view: WorldView, final_deltas: Any, minute: int
    ) -> None:
        """The distress monitor and the day-scale distress, which
        native/core/src/rules/psychology.rs updates for the models with long-term state.

        Nothing for a model other than mean-reverting-distress-v1 and
        mean-reverting-life-state-v1. For those, with each person's stress before (the rows the
        view was made of) and after (`final_deltas`) and the model's `high_distress_threshold`:
        `world.metric_state` gets the five values of `acute_distress_monitor:<id>` (the current
        and the longest high run, the episodes, the total and the count of the runs that ended),
        and `world.model_state["psychological_distress_v1"]` adds the final stress to each
        person's day, with a new record from `baseline_distress_by_agent` (0 by default) for
        someone it has none of. At the last minute of a day each record with observed minutes
        takes its new long-term distress and starts the next day; `last_updated_minute` becomes
        `minute`.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal, before anything changes: a namespace that does not validate or is
        not at the previous minute, deltas of other people or of two for one person, a stress or
        a threshold outside 0..1, a monitor value that is negative or not finite, another model
        with an `update_long_term_state` of its own.
        """

    def _apply_state_deltas(self, world: WorldState, final_deltas: Any) -> None:
        """Write the final deltas, which native/core/src/rules/psychology.rs does.

        Each person's needs, stress and trust take the delta's values, and the body values a
        delta has go to its record of `body_state_v1`. Every delta is checked before anything
        is written.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: two deltas for one person, a person the world does not have, a
        delta of another minute, a value that is not a float within 0..1, body values without a
        body record.
        """

    def _update_movement(self, minute: int) -> None:
        """Advance route interpolation and atomically reschedule changed paths."""
        state = self.world.model_state.get(MOVEMENT_STATE_KEY)
        if state is None:
            return
        self._replan_journeys(state, advance_movement(self.world, minute), minute)

    def _replan_journeys(self, state: Any, agent_ids: Any, minute: int) -> None:
        """New routes for the people whose route changed: native/core/src/rules/replan.rs.

        `agent_ids` are those `advance_movement` returned, in that order; `state` is
        `world.model_state["movement_v1"]`. For each one with a journey: the old trip stops (its
        pending event SUPPRESSED with "the route changed before travel completion", its task gone,
        the person IDLE at the zone of their position with no target, the journey removed), and
        then, from where that left them:

        - a `staff_support_travel` trip walks again to the assigned staff (`_route`,
          `_schedule_travel`) and the support's completion moves after it, or, with no route or no
          time left, the completion is SUPPRESSED, the staff member freed, and the request
          UNRESOLVED (at its timeout) or PENDING again, with a continuity record
          "route_unreachable"; a request no longer ASSIGNED with its task raises
          ValueError("replanned resident trip lost its assigned support task"), a completion not
          the task's one ValueError("replanned resident trip has inconsistent support completion");
        - a `sleep_bed` trip reserves a bed again and walks there, or lies down where it is, or,
          with no bed, releases it; the person is SLEEPING;
        - a trip to a facility in the person's zone takes the room's approach path, and any
          other trip a new route of `_route` to the facility's or destination zone.

        Each person adds 1 to `movement_replan_count`, and 1 to `movement_replan_blocked_count`
        when no route or bed was found.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        before the first stop raises KernelRefusal, and after it a RuntimeError, since a stop
        is written.
        """

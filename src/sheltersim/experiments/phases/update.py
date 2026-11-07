"""The update phase: movement, then each person's psychology and social propagation."""

from __future__ import annotations

from dataclasses import replace
from typing import Any

from sheltersim.core.engine import TraceLevel
from sheltersim.core.rng import KeyedRandom
from sheltersim.core.world import WorldState
from sheltersim.domain.distress_monitor import record_distress_monitor_tick
from sheltersim.domain.movement import MOVEMENT_STATE_KEY, advance_movement
from sheltersim.domain.psychological_state import PSYCHOLOGICAL_STATE_KEY
from sheltersim.experiments.phases.segments import no_python_step
from sheltersim.policies.psychology import PsychologyUpdateResult, apply_state_deltas
from sheltersim.policies.world_view import WorldView


class UpdatePhase:
    """Steps of TickPhase.UPDATE_AGENT_STATE, a mixin of OfflineSimulationController."""

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
        return WorldView.from_world(
            world,
            include_state_hash=False,
            person_cache=self._person_contexts,
            environment_cache=self._zone_environments,
        )

    def _psychology_update(self, view: WorldView, minute: int) -> PsychologyUpdateResult:
        deltas_without_traces = getattr(self.psychology, "deltas_without_traces", None)
        if self.artifacts.trace_level is TraceLevel.OFF and deltas_without_traces is not None:
            # No psychology trace is recorded, so its per-agent parts are not built.
            psychology_result = PsychologyUpdateResult(deltas_without_traces(view, minute), ())
        else:
            psychology_result = self.psychology.explain_update(view, minute)
        return psychology_result

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
        raise no_python_step("_social_propagation")

    def _record_psychology_trace(
        self, psychology_result: PsychologyUpdateResult, final_deltas: Any, minute: int
    ) -> None:
        base_deltas = psychology_result.deltas
        if self.artifacts.trace_level is not TraceLevel.OFF:
            final_by_agent = {delta.agent_id: delta for delta in final_deltas}
            base_by_agent = {delta.agent_id: delta for delta in base_deltas}
            traced_result = replace(
                psychology_result,
                traces=tuple(
                    replace(
                        trace,
                        stress_after=final_by_agent[trace.agent_id].stress,
                        social_stress_delta=(
                            final_by_agent[trace.agent_id].stress
                            - base_by_agent[trace.agent_id].stress
                        ),
                    )
                    for trace in psychology_result.traces
                ),
            )
            self.artifacts.record_psychology_trace(
                traced_result.trace_document(
                    minute,
                    source_id=self.psychology.source_id,
                    full=self.artifacts.trace_level is TraceLevel.FULL,
                ),
                agent_contributions=(
                    {
                        "agent_id": trace.agent_id,
                        "stress_before": trace.stress_before,
                        "stress_after": final_by_agent[trace.agent_id].stress,
                        "raw_stress_delta": trace.raw_stress_delta,
                        "social_stress_delta": (
                            final_by_agent[trace.agent_id].stress
                            - base_by_agent[trace.agent_id].stress
                        ),
                        "component_contributions": dict(trace.component_contributions),
                    }
                    for trace in psychology_result.traces
                ),
            )

    def _update_long_term_psychology(
        self, world: WorldState, view: WorldView, final_deltas: Any, minute: int
    ) -> None:
        update_long_term = getattr(self.psychology, "update_long_term_state", None)
        if callable(update_long_term):
            record_distress_monitor_tick(
                world.metric_state,
                previous_by_agent={
                    agent_id: agent.stress for agent_id, agent in view.agents.items()
                },
                current_by_agent={delta.agent_id: delta.stress for delta in final_deltas},
                threshold=self.psychology.parameters.high_distress_threshold,
            )
            runtime_state = world.model_state.get(PSYCHOLOGICAL_STATE_KEY)
            if not isinstance(runtime_state, dict):
                raise ValueError("mean-reverting psychology runtime state is missing")
            world.model_state[PSYCHOLOGICAL_STATE_KEY] = update_long_term(
                runtime_state, minute, final_deltas
            )

    def _apply_state_deltas(self, world: WorldState, final_deltas: Any) -> None:
        apply_state_deltas(world, final_deltas)

    def _update_movement(self, minute: int) -> None:
        """Advance route interpolation and atomically reschedule changed paths."""
        state = self.world.model_state.get(MOVEMENT_STATE_KEY)
        if state is None:
            return
        self._replan_journeys(state, advance_movement(self.world, minute), minute)

    def _replan_journeys(self, state: Any, agent_ids: Any, minute: int) -> None:
        """New routes for the people whose route changed, which native/src/rules/replan.rs plans.

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
        raise no_python_step("_replan_journeys")

"""The arbitrate phase: services and staff take up what the proposals asked for."""

from __future__ import annotations

from sheltersim.core.world import WorldState


class ArbitratePhase:
    """Steps of TickPhase.ARBITRATE_ACTIONS, a mixin of OfflineSimulationController.

    The driver runs the phase in native/core/src/rules/tick/arbitrate.rs, in this order:
    `update_staff_dynamics` of domain/staff_dynamics.py, `update_staff_operations` of
    domain/staff_operations.py, the services (`toilet_service.arbitrate(minute, rng)`, then
    `resource_service.arbitrate(minute, rng)` when there is one), `_assign_staff_support`,
    `assign_staff_operations` with `staff_space` (direct support takes its staff before the
    background patrol, outreach, sanitation and maintenance work), `_consider_due_complaints`, and
    `schedule_staff_operation_relief`.
    """

    def _assign_staff_support(self, world: WorldState, minute: int) -> None:
        """`staff_service.assign_pending(minute, validate_state=False)`, then the care receipts of
        what it assigned, which native/src/rules/support.rs writes.

        For each request assigned whose care receipt exists and that has a staff member, an
        "assigned" care transition, and a "started" one when its person is SEEKING_HELP: with the
        receipt's household, not the current one (#179), the receipt's kind, the request's task as
        the causation, the assigned staff, and the person's sources.

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS). A receipt it cannot
        write after the service has assigned is a RuntimeError.
        """

    def _consider_due_complaints(self, minute: int) -> None:
        """Ask `social_policy` about each request at the minute its wait reaches the threshold,
        which native/src/rules/staff.rs does.

        Nothing without `social_policy`. Otherwise the requests of
        `world.unsettled_support_requests()` that are PENDING and whose `requested_minute` plus
        `social_policy.limits.complaint_wait_minutes` is `minute`, in ID order: each one still so
        when its turn comes (a decision before it may have settled it) goes to
        `social_policy.consider_complaint_response(world, agent_id=..., request_id=...)`, which
        stays Python (the LLM boundary).

        The kernel alone runs this step (segments.py, KERNEL_ONLY_SEGMENTS); what it turns down
        raises KernelRefusal: a threshold or minute that is not an int, a request with no row.
        """

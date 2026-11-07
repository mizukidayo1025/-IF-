"""The arbitrate phase: services and staff take up what the proposals asked for."""

from __future__ import annotations

from sheltersim.core.rng import KeyedRandom
from sheltersim.core.world import WorldState
from sheltersim.domain.staff_dynamics import update_staff_dynamics
from sheltersim.domain.staff_operations import (
    assign_staff_operations,
    schedule_staff_operation_relief,
    update_staff_operations,
)


class ArbitratePhase:
    """Steps of TickPhase.ARBITRATE_ACTIONS, a mixin of OfflineSimulationController."""

    def _run_arbitrate_phase(self, world: WorldState, minute: int, rng: KeyedRandom) -> None:
        update_staff_dynamics(world, minute)
        update_staff_operations(world, minute)
        self._arbitrate_services(minute, rng)
        self._assign_staff_support(world, minute)
        # Reserve eligible staff for direct occupant support before background patrol,
        # outreach, sanitation and maintenance work consumes the remaining staff.
        assign_staff_operations(world, minute, self.staff_space)
        self._consider_due_complaints(minute)
        schedule_staff_operation_relief(world, minute)

    def _arbitrate_services(self, minute: int, rng: KeyedRandom) -> None:
        self.toilet_service.arbitrate(minute, rng, validate_state=False)
        if self.resource_service is not None:
            self.resource_service.arbitrate(minute, rng, validate_state=False)

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

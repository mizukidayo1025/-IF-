"""Installed alternative numerical operators; no evaluation of arbitrary code.

The kernels of native/src/rules know each class here by identity.
"""

from __future__ import annotations

import math
from dataclasses import replace
from typing import Any

from sheltersim.policies.information import InformationSpreadPolicy
from sheltersim.policies.psychology import PsychologyUpdateResult, SimplePsychologyPolicy
from sheltersim.policies.social import StressPropagationPolicy


class BoundedStressPolicy(SimplePsychologyPolicy):
    """Exponential pressure/recovery avoids a linear stress saturation step."""

    policy_version = "bounded-stress-v1"

    def explain_update(self, view: Any, minute: int) -> PsychologyUpdateResult:
        baseline = super().explain_update(view, minute)
        deltas, traces = [], []
        for delta, trace in zip(baseline.deltas, baseline.traces, strict=True):
            components = [
                trace.need_pressure_stress,
                trace.queue_wait_stress,
                trace.support_wait_stress,
                trace.environmental_stress,
            ]
            pressure = sum(components)
            added = (1 - trace.stress_before) * -math.expm1(-pressure)
            scale = added / pressure if pressure else 0
            recovery = trace.stress_before * -math.expm1(trace.recovery_stress)
            stress = min(1.0, max(0.0, trace.stress_before + added - recovery))
            deltas.append(replace(delta, stress=stress))
            traces.append(
                replace(
                    trace,
                    stress_after=stress,
                    need_pressure_stress=components[0] * scale,
                    queue_wait_stress=components[1] * scale,
                    support_wait_stress=components[2] * scale,
                    environmental_stress=components[3] * scale,
                    recovery_stress=-recovery,
                )
            )
        return PsychologyUpdateResult(
            tuple(deltas), tuple(traces), policy_version=self.policy_version
        )


class DeGrootContactPolicy(StressPropagationPolicy):
    """Sparse consensus permits calming and amplification from the same contacts.

    The contacts, their minutes and their draws are StressPropagationPolicy's. Each person moves
    toward the mean stress of the contacts that have lasted `minimum_contact_minutes`, by
    `min(1, stress_spread_per_minute)`, capped per pair and per person, after recovering by
    `natural_recovery_per_minute`; there is no cooldown. Its step is
    native/src/rules/social_propagation.rs.
    """

    policy_version = "degroot-contact-v1"


class HazardInformationPolicy(InformationSpreadPolicy):
    """InformationSpreadPolicy with a hazard: a transfer's probability is `1 - exp(-p)` of the
    sparse model's `p`. Its step is native/src/rules/information.rs."""

    policy_version = "hazard-information-v1"

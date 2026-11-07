"""Compare the kernels of the handlers' rules with their Python rules at each call.

The seeks of rules/proposals.rs (the rules of rules/care_receipts.rs, rules/toilet.rs,
rules/support.rs and `after_arrival` of rules/travel_completion.rs have no Python rule left, lane
L10). The Python rule runs first on a copy of everything it writes (shadow_service.py's, with the
receipts, the requests and the body state too), and the kernel then runs on the world. All of it
must come out the same, floats by their bits, with each event's repr and each record. What the
kernel refuses isn't compared: the Python rule runs on the world then. `seek_help` and
`seek_information` are `seek` here, which shadow_proposals.py calls.

The hooks of the LLM (`consider_support_request`, `_prioritize_support_request`) record what they
decide, so the copy leaves them out: they are the same Python on both sides of the port. With a
Provider that isn't offline they can schedule events and read the tape, so a `seek` is then not
compared beyond its result: the hashes of a tape's Run say whether it went the same.
"""

from __future__ import annotations

import json
from types import SimpleNamespace
from typing import Any

from shadow_kit import Recorder, text, values_in
from shadow_service import _Log, _ServiceTwin, compare_after, controller_stand_in

SHARED = "shared_operations_v1"
BODY = "body_state_v1"
INFORMATION = "information_spread_v1"
ROW_CHECK_EVERY = 100
_seek: list[Any] = []


class _CareLog(_Log):
    """The log for a Python rule: the world's, with what it appended."""

    def __init__(self, log: Any) -> None:
        super().__init__(log)
        self.world_log = log

    def find(self, event_id: str) -> Any:
        for record in self.appended:
            if record.event.event_id == event_id:
                return record
        return self.world_log.find(event_id)

    def has_prior_care_request(self, request_id: str, event_id: str) -> bool:
        return self.world_log.has_prior_care_request(request_id, event_id)

    def completed_support_kinds(self, agent_id: str) -> Any:
        return self.world_log.completed_support_kinds(agent_id)


class _HandlerTwin(_ServiceTwin):
    def __init__(self, world: Any) -> None:
        super().__init__(world)
        fields = self.__dict__
        table = fields["model_state"]
        shared = world.model_state.get(SHARED)
        if shared is not None:
            table[SHARED] = {
                key: list(value) if key == "history" else json.loads(json.dumps(value))
                for key, value in shared.items()
            }
        body = world.model_state.get(BODY)
        if body is not None:
            table[BODY] = {
                **body,
                "agents": {key: dict(value) for key, value in body["agents"].items()},
            }
        information = world.model_state.get(INFORMATION)
        if information is not None:
            table[INFORMATION] = json.loads(json.dumps(information))
        fields["event_log"] = _CareLog(world.event_log)
        fields["support_requests"] = {
            key: SimpleNamespace(**vars(item)) for key, item in world.support_requests.items()
        }


def _compare_more(recorder: Recorder, where: str, world: Any, twin: Any) -> None:
    """What shadow_service.py's comparison leaves out."""
    fields = twin.__dict__
    table = fields["model_state"]
    seen = recorder.seen
    shared = world.model_state.get(SHARED)
    if shared is not None:
        theirs = table[SHARED]
        count = len(theirs["history"])
        if len(shared["history"]) != count or text(shared["history"][-4:]) != text(
            theirs["history"][-4:]
        ):
            recorder.differ(f"{where}: the receipts {text(shared['history'][-1:]):.400}")
        for key in shared:
            if key != "history" and text(shared[key]) != text(theirs[key]):
                recorder.differ(f"{where}: {SHARED}.{key}")
        seen["values_compared"] += values_in(theirs["support_requests"]) + 15
    body = world.model_state.get(BODY)
    if body is not None:
        if text(body["agents"]) != text(table[BODY]["agents"]):
            recorder.differ(f"{where}: the body records")
        seen["values_compared"] += 6 * len(body["agents"])
    information = world.model_state.get(INFORMATION)
    if information is not None:
        for key in ("query_pressure", "support_requests", "counters"):
            if text(information[key]) != text(table[INFORMATION][key]):
                recorder.differ(f"{where}: {INFORMATION}.{key}")
    for key, agent in world.agents.items():
        other = fields["agents"][key]
        if (
            agent.support_request_id != other.support_request_id
            or agent.support_cooldown_until != other.support_cooldown_until
            or agent.next_decision_at != other.next_decision_at
            or agent.zone_id != other.zone_id
        ):
            recorder.differ(f"{where}: {key}'s request or decision")
    if list(world.support_requests) != list(fields["support_requests"]):
        recorder.differ(f"{where}: the requests' order")
    for key, request in world.support_requests.items():
        other = fields["support_requests"][key]
        if repr(_request_values(request)) != repr(_request_values(other)):
            recorder.differ(f"{where}: request {key}")


def _request_values(request: Any) -> tuple[Any, ...]:
    status = request.status
    return (
        request.id,
        request.agent_id,
        request.requested_minute,
        request.timeout_minute,
        getattr(status, "value", status),
        request.assigned_staff_id,
        request.task_id,
        request.resolved_minute,
        request.cooldown_until,
        request.last_assignment_attempt_minute,
        request.last_assignment_wait_reason_code,
    )


def prepare(output: str) -> None:
    from sheltersim import native

    recorder = Recorder(output, "handlers")
    unsupported = native.Unsupported

    def row_check(world: Any, where: str) -> None:
        recorder.seen["row_checks.due"] += 1
        if recorder.seen["row_checks.due"] % ROW_CHECK_EVERY:
            return
        recorder.seen["row_checks.made"] += 1
        problem = world._core.entities().verify(world)
        if problem:
            recorder.differ(f"{where}: the rows and the objects: {problem}")
        for name in (SHARED, BODY):
            if name in world.model_state and not world._core.mirror(name).verify():
                recorder.differ(f"{where}: the row and the tree of {name}")

    def compared(
        label: str,
        kernel: Any,
        python_rule: Any,
        world_of: Any,
        result_text: Any = text,
        detail: Any = None,
        hooks_run: Any = None,
    ) -> Any:
        """A stand-in for a writing kernel. python_rule(twin, *arguments, **options) is the rule."""

        def run(*arguments: Any, **options: Any) -> Any:
            if _is_copy(arguments):
                raise unsupported("a shadow's copy")
            world = world_of(*arguments)
            recorder.seen[f"{label}.calls"] += 1
            twin = _HandlerTwin(world)
            pending_before = len(world.event_queue.pending())
            error = None
            expected = None
            try:
                expected = python_rule(twin, *arguments, **options)
            except Exception as raised:  # noqa: BLE001 - only compared with what the kernel does
                error = raised
            where = f"{label} {arguments[1:]!r:.160} {options!r:.200} at {world.minute_index}"
            try:
                result = kernel(*arguments, **options)
            except unsupported as reason:
                recorder.reasons[f"{label}: {reason}"] += 1
                recorder.seen[
                    f"{label}.unsupported_python_{'raised' if error else 'accepted'}"
                ] += 1
                raise
            except Exception as raised:
                recorder.seen[f"{label}.kernel_ran"] += 1
                recorder.seen[f"{label}.raised.{type(raised).__name__}"] += 1
                if error is None or type(error) is not type(raised) or str(error) != str(raised):
                    recorder.differ(f"{where}: raised {raised!r:.200}, and Python {error!r:.200}")
                raise
            recorder.seen[f"{label}.kernel_ran"] += 1
            if error is not None:
                recorder.differ(f"{where}: the kernel took what Python turned down: {error!r:.300}")
                return result
            if result_text(result) != result_text(expected):
                recorder.differ(f"{where}: returned {result!r:.200}, not {expected!r:.200}")
            if hooks_run is not None and hooks_run(*arguments):
                recorder.seen[f"{label}.with_provider_hooks_result_only"] += 1
            else:
                compare_after(recorder, where, world, twin, pending_before)
                _compare_more(recorder, where, world, twin)
            if detail is not None:
                detail(result, arguments, options)
            row_check(world, where)
            return result

        return run

    # `care_receipts.append_transition` has no Python rule left to compare (`kernel_only`,
    # lane L10).

    # rules/toilet.rs (the request, the facilities to serve, the room, the head's start and the
    # completion), rules/support.rs (the end and the timeout of support) and `after_arrival` of
    # rules/travel_completion.rs have no Python rule left to compare (`kernel_only`, lane L10), nor
    # have the steps of experiments/phases/dispatch.py that follow an event.

    def seek_rule(twin: Any, controller: Any, proposal: Any) -> Any:
        stand_in = controller_stand_in(controller, twin)
        stand_in.llm_router = None
        stand_in.social_policy = None
        service = type(controller.staff_service).__new__(type(controller.staff_service))
        service.__dict__.update(controller.staff_service.__dict__)
        service.world = twin
        stand_in.staff_service = service
        return stand_in._apply_effect(proposal)

    def seek_detail(result: Any, arguments: Any, _options: Any) -> None:
        action = arguments[1].action.value
        recorder.seen[f"seek.{action}.{'opened' if result[0] else 'rejected'}"] += 1

    _seek.append((compared, seek_rule, seek_detail))


def _is_copy(arguments: Any) -> bool:
    for item in arguments:
        if isinstance(item, _ServiceTwin) or isinstance(getattr(item, "world", None), _ServiceTwin):
            return True
    return False


def seek_shadow(kernel: Any) -> Any:
    """What shadow_proposals.py runs for `seek_help` and `seek_information`."""
    compared, rule, detail = _seek[0]

    def hooks_run(controller: Any, _proposal: Any) -> bool:
        router = controller.llm_router
        return router is not None and router.mode.value != "offline"

    return compared(
        "seek",
        kernel,
        rule,
        lambda controller, _proposal: controller.world,
        lambda result: text(list(result)),
        detail,
        hooks_run,
    )

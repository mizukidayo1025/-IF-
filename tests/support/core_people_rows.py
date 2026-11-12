"""The fixture of the core's tests of the people's rows (Issue #225, lane M5).

`native/core/tests/action_lifecycle_rows.rs`, `social_network_rows.rs` and `body_state_rows.rs`
check the kernels of `agent_dynamics_v1`, the open actions, `social_network_v1`, the social dialogue
and `body_state_v1` (`rules::agent_dynamics`, `action_lifecycle`, `social_network`,
`social_dialogue`, `body_state`, and the social action of `rules::social_interaction`) on the worlds
the pytest files of the same names built: the Project screen's generated Scenario (20 people), at
minute 0 and after 40 ticks, edited as those files edited it. Their inputs made with Python's
`random.Random` are written here as the files made them, call by call.

`people_rows.json` is `{"worlds": ..., "cases": ..., "canonical_sha256": ..., "inputs": ...}`, a
file of `core_texts.Fixture` (worlds as variants of others, and the cases of the social action's
texts, `native/core/tests/support/cases.rs`), with `inputs`:

- `events`: the events the tests apply or log, by name (`ScheduledEvent.to_dict()`).
- `writers`: the 400 random calls of the agent_dynamics writers, each `[kind, agent_id,
  facility_id, action, target_id]` (`kind`: `propose`, `learn`, `announce`, `knows`, or `none`, a
  step that only reads the namespace).
- `sequence`: the 300 random proposals of the open actions, each `[minute, agent_id, fatigue,
  action, target_id, committed]` (`fatigue` None where the step did not set it).
- `network`: the 1500 random events of `observe_event` (`_network_events`), and `formula`: the 400
  random values of the social action's formula (`_formula_values`), each the JSON text of a list,
  on one line, to keep the file small.
- `body_payloads` and `random_body_events`: the events of the body state's tests, each
  `[event_type, payload]`.
- `expected`: what the pytest files computed in Python for a test (`known_sorted`), and the order
  of the finished world's relationships in `social_network_v1` (`network_order`), as the Run left
  them, and of the keys of the body state's config (`body_config_order`), as its rule made the
  dict; a canonical document sorts both.

The finished world (after 40 ticks) is written without its event log, which none of these rules
reads, to keep the file small.
"""

from __future__ import annotations

import copy
import json
import random
import tempfile
from pathlib import Path
from typing import Any

from support.core_fixtures import run_world
from support.core_texts import Fixture, handler_of

# The minutes the social network's tests ran the generated Scenario for.
FINISHED_MINUTES = 40
SOCIAL_SOURCE_ID = "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1"
SOCIAL_ACTIONS = {
    "reassurance_exchange": ("offer_reassurance", "emotional_check_in"),
    "family_care": ("provide_family_support", "family_care"),
    "voluntary_support": ("offer_practical_help", "mutual_aid"),
}
BODY = "body_state_v1"
DYNAMICS = "agent_dynamics_v1"


def _scenario() -> Any:
    from support.generated import preview_scenario

    return preview_scenario(Path(tempfile.mkdtemp(prefix="core-people-rows-")))


def _scheduled(
    event_id: str, event_type: str, sequence: int, due: int, **payload: Any
) -> dict[str, Any]:
    """`_event` of test_action_lifecycle_rows.py."""
    from sheltersim.events.models import EventKind, ScheduledEvent

    event = ScheduledEvent(event_id, EventKind.COMPLETION, due, 0, sequence, event_type, payload)
    return event.to_dict()


# ---- integration/test_action_lifecycle_rows.py -------------------------------------------------


def _writers(world: Any) -> list[list[Any]]:
    """The random calls of `test_the_writers_are_the_python_rules_on_random_calls`."""
    from sheltersim.policies.behavior import ActionKind

    rng = random.Random(7)
    agents = sorted(world.agents)
    facilities = sorted(world.facilities)
    steps: list[list[Any]] = []
    for step in range(400):
        agent_id = rng.choice([*agents, "nobody"])
        facility_id = rng.choice([*facilities, "nowhere"])
        kind = step % 4
        if kind == 0:
            action = rng.choice(list(ActionKind)).value
            target = rng.choice([None, facility_id])
            steps.append(["propose", agent_id, facility_id, action, target])
        elif kind == 1:
            steps.append(["learn", agent_id, facility_id, None, None])
        elif kind == 2:
            announce = rng.random() < 0.05
            steps.append(["announce" if announce else "none", agent_id, facility_id, None, None])
        else:
            steps.append(["knows", agent_id, facility_id, None, None])
    return steps


def _sequence(world: Any) -> list[list[Any]]:
    """The random proposals of `test_a_sequence_of_actions_gives_the_python_rules_receipts`."""
    from sheltersim.policies.behavior import ActionKind

    rng = random.Random(11)
    agents = sorted(world.agents)[:6]
    actions = [ActionKind.WAIT, ActionKind.REST, ActionKind.SLEEP, ActionKind.FETCH_WATER]
    steps: list[list[Any]] = []
    for minute in range(300):
        agent_id = rng.choice(agents)
        fatigue = rng.random() if rng.random() < 0.3 else None
        target = rng.choice([None, None, "T1"])
        action = rng.choice(actions).value
        committed = rng.random() < 0.9
        steps.append([minute, agent_id, fatigue, action, target, committed])
    return steps


# The events of `test_an_applied_event_gives_the_python_rules_receipt`.
APPLIED = [
    ("toilet_service_complete", "request_toilet", {"facility_id": "T1"}),
    ("resource_service_complete", "fetch_water", {"facility_id": "T1"}),
    ("staff_support_complete", "seek_help", {"request_id": "T1", "staff_id": "S"}),
    ("support_request_timeout", "seek_help", {"request_id": "T1"}),
    ("social_interaction", "socialize", {"participant_agent_id": "T1"}),
    ("facility_queue_abandon", "collect_meal", {"facility_id": "T1"}),
    ("resource_service_cancel", "collect_meal", {"facility_id": "T1"}),
    ("resource_service_cancel", "collect_meal", {"facility_id": "other"}),
    ("social_interaction", "rest", {"participant_agent_id": "T1"}),
    ("agent_travel_complete", "rest", {"facility_id": "T1"}),
]


def _lifecycle(fixture: Fixture, inputs: dict[str, Any], scenario: Any) -> None:
    world, controller, _ = run_world(scenario)
    fixture.world("start", world, controller, full=True)
    agent_id = sorted(world.agents)[0]
    facilities = sorted(world.facilities)
    events = inputs["events"]
    for index, (event_type, _, keys) in enumerate(APPLIED):
        for sender in (False, True):
            who = {"sender_agent_id": agent_id} if sender else {"agent_id": agent_id}
            events[f"applied.{int(sender)}.{index}"] = _scheduled(
                "e-1", event_type, 900, 3, **who, **keys
            )
    events["early"] = _scheduled("e-early", "resource_service_complete", 903, 2, agent_id=agent_id)
    events["nobody"] = _scheduled("e-no", "resource_service_complete", 904, 7, agent_id=5)
    events["gone"] = _scheduled("e-1", "resource_service_cancel", 901, 6, agent_id=agent_id)
    events["population"] = _scheduled("pop-1", "effect_program_population", 900, 3)
    events["population_other"] = _scheduled(
        "o-1", "toilet_service_complete", 901, 3, agent_id=sorted(world.agents)[0]
    )
    events["taken"] = _scheduled("taken", "other_type", world.id_counters.get("event", 0), 0)
    inputs["sequence"] = _sequence(world)

    # `test_the_writers_are_the_python_rules_on_random_calls`: the announced policy, and the first
    # person knowing nothing.
    announced = copy.deepcopy(world)
    from sheltersim.domain.agent_dynamics import ANNOUNCED_FACILITY_AGENT_DYNAMICS_POLICY_VERSION

    announced.model_configs["agent_dynamics"] = {
        "policy_version": ANNOUNCED_FACILITY_AGENT_DYNAMICS_POLICY_VERSION
    }
    announced.model_state[DYNAMICS]["agents"][sorted(world.agents)[0]]["known_facility_ids"] = []
    fixture.world("announced", announced, controller)
    inputs["writers"] = _writers(world)

    # `test_a_known_list_out_of_order_is_sorted_as_the_python_rule_does`: what Python expected.
    two = facilities[:2]
    discovered = {two[0]}
    awareness = world.model_configs.get("facility_awareness")
    if isinstance(awareness, dict) and awareness.get(
        "discover_co_located_facilities_on_arrival", True
    ):
        zone = world.facilities[two[0]].zone_id
        discovered |= {item.id for item in world.facilities.values() if item.zone_id == zone}
    inputs["expected"]["known_sorted"] = sorted(set(two) | discovered)

    # `test_a_population_exit_gives_the_python_rules_receipts`: the namespaces it wrote.
    agents = sorted(world.agents)
    inputs["population"] = {
        "population_lifecycle_v1": {
            "history": [
                {"event_id": "pop-1", "action": "temporary_exit", "agent_id": agents[3]},
                {"event_id": "pop-1", "action": "transferred", "agent_id": agents[1]},
                {"event_id": "pop-1", "action": "returned", "agent_id": agents[0]},
                {"event_id": "other", "action": "transferred", "agent_id": agents[0]},
                "not a dict",
            ]
        },
        "population_dynamics_v1": {
            "departures": [{"event_id": "pop-1", "agent_id": agents[1]}, {"event_id": "pop-2"}]
        },
    }


# ---- integration/test_social_network_row.py ----------------------------------------------------


def _social_event(event_type: str, payload: dict[str, Any], minute: int, number: int = 1) -> Any:
    """`_event` of test_social_network_row.py."""
    from sheltersim.events.models import EventKind, ScheduledEvent

    return ScheduledEvent(
        event_id=f"e{number}",
        kind=EventKind.ENDOGENOUS,
        due_minute=minute,
        priority=0,
        sequence=number,
        event_type=event_type,
        payload=payload,
        source_id="policy-v1",
        provenance={
            "source_id": SOCIAL_SOURCE_ID,
            "source_type": "assumption",
            "policy_version": "policy-v1",
        },
    )


def _action(world: Any, kind: str, actor: str, participant: str, **more: Any) -> Any:
    action, topic = SOCIAL_ACTIONS[kind]
    return _social_event(
        "social_interaction",
        {
            "interaction_type": kind,
            "agent_id": actor,
            "participant_agent_id": participant,
            "zone_id": world.agents[actor].zone_id,
            "topic_id": topic,
            "action": action,
            "reason_short": "autonomous_need_choice",
            "interaction_duration_minutes": 5,
            **more,
        },
        world.minute_index,
    )


def _network_events() -> list[str]:
    """The random draws of `test_observe_event_gives_the_bits_of_the_python_rule`, call by call:
    the JSON text of `[positive_affect, negative_affect, kind, first, second, message_id]`, where
    the people are indices of the first six by ID, `kind` is the draw that picks the event
    (`_random_event`: 0 a contact with `message_id` or None, 1 a proximity contact, 2 a
    correction, 3 to 6 a social interaction of reassurance, family care, voluntary support and
    cooperation), and `message_id` is None for any other kind."""
    rng = random.Random(11)
    ids = list(range(6))
    out: list[str] = []
    for number in range(1, 1501):
        positive, negative = rng.random(), rng.random()
        first, second = rng.sample(ids, 2)
        kind = rng.randrange(7)
        message = rng.choice([None, f"m{rng.randrange(80)}", f"m{number}"]) if kind == 0 else None
        item = [positive, negative, kind, first, second, message]
        out.append(json.dumps(item, separators=(",", ":")))
    return out


def _formula_values() -> list[str]:
    """The random values of `test_a_social_action_gives_the_bits_of_the_rule_s_formula`, each the
    JSON text of `[stress, trust, fatigue, positive_affect, negative_affect]`."""
    rng = random.Random(3)
    values = []
    for _ in range(400):
        stress, trust, fatigue = rng.random(), rng.random(), rng.random() * 0.2
        positive, negative = rng.random(), rng.random()
        values.append(json.dumps([stress, trust, fatigue, positive, negative]))
    return values


def _household(world: Any, ids: list[str], households: list[str]) -> None:
    presence = world.model_state.get("population_presence_v1")
    for id_, household in zip(ids, households):
        if presence is not None:
            presence["people"].setdefault(id_, {})["household_id"] = household
        else:
            world.agents[id_].profile.group_id = household


def _colocate(world: Any, ids: list[str]) -> str:
    zone = world.agents[ids[0]].zone_id
    for id_ in ids:
        world.agents[id_].zone_id = zone
    return zone


def _social(fixture: Fixture, inputs: dict[str, Any], scenario: Any) -> None:
    from sheltersim.domain.models import ordered_pair_key
    from sheltersim.events.handlers import SocialInteractionHandler
    from sheltersim.experiments.scenario import ScenarioSpec

    document = scenario.model_dump(mode="json")
    document["duration_minutes"] = FINISHED_MINUTES
    world, controller, _ = run_world(ScenarioSpec.model_validate(document), FINISHED_MINUTES)
    fixture.world("finished", world, controller)
    _drop_log(fixture, "finished")
    first, second = sorted(world.agents)[:2]
    inputs["network"] = _network_events()
    # The order of the relationships as the Run left them, which a canonical document sorts.
    inputs["expected"]["network_order"] = list(
        world.model_state["social_network_v1"]["relationships"]
    )
    inputs["formula"] = _formula_values()

    # `test_observe_event_computes_trust_when_the_body_has_no_record`: the pair's records gone.
    unknown = copy.deepcopy(world)
    for key in (ordered_pair_key(first, second), ordered_pair_key(second, first)):
        unknown.model_state[BODY]["relationships"].pop(key, None)
        unknown.model_state["social_network_v1"]["relationships"].pop(key, None)
    fixture.world("unknown_pair", unknown, controller)
    _drop_log(fixture, "unknown_pair")

    # The social action: the pair in one zone and household.
    pair = copy.deepcopy(world)
    _colocate(pair, [first, second])
    _household(pair, [first, second], ["h1", "h1"])
    fixture.world("pair", pair, controller)
    _drop_log(fixture, "pair")
    inputs["events"].update(
        {f"action.{kind}": _action(pair, kind, first, second).to_dict() for kind in SOCIAL_ACTIONS}
    )

    handler = handler_of(SocialInteractionHandler())
    zone = world.agents[first].zone_id
    group = "a_suppressed_action_says_python_s_text_with_nothing_written"
    apart = copy.deepcopy(world)
    _colocate(apart, [first, second])
    event = _action(apart, "reassurance_exchange", first, second)
    apart.agents[second].zone_id = next(name for name in apart.zones if name != zone)
    name = fixture.world("apart", apart, controller)
    _drop_log(fixture, name)
    fixture.says(group, name, handler, event, "social action participants are no longer co-located")

    group = "a_helper_who_needs_more_than_the_target_is_suppressed"
    needy = copy.deepcopy(world)
    _colocate(needy, [first, second])
    needy.agents[first].stress = 0.9
    needy.agents[second].stress = 0.1
    for need in ("toilet_urgency", "hunger", "fatigue", "hydration_need"):
        setattr(needy.agents[second].needs, need, 0.1)
    name = fixture.world("needy_helper", needy, controller)
    _drop_log(fixture, name)
    fixture.says(
        group,
        name,
        handler,
        _action(needy, "voluntary_support", first, second),
        "the selected person no longer has a higher immediate need",
    )

    group = "a_different_household_is_suppressed"
    households = copy.deepcopy(world)
    _colocate(households, [first, second])
    _household(households, [first, second], ["h1", "h2"])
    name = fixture.world("two_households", households, controller)
    _drop_log(fixture, name)
    fixture.says(
        group,
        name,
        handler,
        _action(households, "family_care", first, second),
        "family support target is no longer in the same household",
    )

    group = "a_bad_payload_is_failed_by_the_kernel_in_python_s_text"
    together = copy.deepcopy(world)
    _colocate(together, [first, second])
    name = fixture.world("together", together, controller)
    _drop_log(fixture, name)
    duration = "social action duration must be within 1..1440 minutes"
    for more, text in (
        ({"interaction_duration_minutes": 0}, duration),
        ({"interaction_duration_minutes": True}, duration),
        ({"reason_short": "other"}, "social action reason code is not allowed"),
        ({"action": "other"}, "social action does not match its typed topic"),
        ({"extra": 1}, "autonomous social action payload fields do not match"),
        (
            {"conversation_state_context": {"schema_version": 2}},
            "social dialogue state context is invalid",
        ),
    ):
        event = _action(together, "reassurance_exchange", first, second, **more)
        fixture.says(group, name, handler, event, text)


def _drop_log(fixture: Fixture, name: str) -> None:
    """The world without its event log, which none of these rules reads."""
    world = fixture.worlds[name]
    if "base" in world:
        world["changes"] = [
            change for change in world["changes"] if change[1][:2] != ["world", "event_log"]
        ]
    else:
        world["world"]["event_log"] = []
    fixture.entries[name]["world"]["event_log"] = []


# ---- integration/test_body_state_row.py --------------------------------------------------------


def _body_world(scenario: Any, **selection: str) -> Any:
    """`_world` of test_body_state_row.py."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.body_state import create_body_state

    world = WorldState.from_scenario(scenario)
    world.model_state[BODY] = create_body_state(world)
    if selection:
        world.model_configs = {
            **world.model_configs,
            "policy_registry": {
                "schema_version": 2,
                "psychology": "mean-reverting-life-state-v1",
                **selection,
            },
        }
    return world


def _payloads(first: str, second: str) -> list[list[Any]]:
    """`_payloads` of test_body_state_row.py."""
    interaction = {"agent_id": first, "participant_agent_id": second}
    return [
        [
            "proximity_contact_update",
            {
                "first_agent_id": first,
                "second_agent_id": second,
                "observed_minute": 9,
                "distance_m": 1.5,
                "contact_minutes": 3,
            },
        ],
        [
            "proximity_contact_update",
            {"first_agent_id": first, "second_agent_id": second, "observed_minute": 12},
        ],
        [
            "information_contact_update",
            {
                "sender_agent_id": first,
                "recipient_agent_id": second,
                "message_id": "M1",
                "contact_minutes": 2.5,
            },
        ],
        ["information_contact_update", {"sender_agent_id": second, "recipient_agent_id": first}],
        [
            "social_interaction",
            {
                **interaction,
                "interaction_type": "cooperation",
                "topic_id": "T1",
                "interaction_duration_minutes": 7,
            },
        ],
        *[
            [
                "social_interaction",
                {
                    **interaction,
                    "interaction_type": kind,
                    "topic_id": "T2",
                    "interaction_duration_minutes": 0,
                },
            ]
            for kind in ("reassurance_exchange", "family_care", "voluntary_support")
        ],
        [
            "social_interaction",
            {"agent_id": first, "interaction_type": "complaint_response", "request_id": "R"},
        ],
        ["social_interaction", {**interaction, "interaction_type": "something_else"}],
        ["social_interaction", {"agent_id": "nobody", "interaction_type": "complaint_response"}],
        ["information_contact_update", {"sender_agent_id": first, "recipient_agent_id": first}],
        ["information_contact_update", {"sender_agent_id": first, "recipient_agent_id": "nobody"}],
        ["proximity_contact_update", {"first_agent_id": first, "second_agent_id": second}],
    ]


AFFECTS = ["linear-positive-negative-v1", "trust-weighted-positive-negative-v1"]
RELATIONSHIPS = ["distance-duration-repeat-trust-v1", "duration-contact-trust-v1"]


def _body(fixture: Fixture, inputs: dict[str, Any], scenario: Any) -> None:
    from sheltersim.domain.information import create_information_runtime_state
    from sheltersim.domain.models import AgentInformationState

    base = _body_world(scenario)
    fixture.world("body", base, None)
    first, second = sorted(base.agents)[:2]
    inputs["body_payloads"] = _payloads(first, second)
    # The order of the body config's keys, as the rule made the dict; a canonical document sorts it.
    inputs["expected"]["body_config_order"] = list(base.model_state[BODY]["config"])

    # `test_each_event_is_python_s_bit_for_bit`: each pair of models, two people's random records.
    for affect_index, affect in enumerate(AFFECTS):
        for relationship_index, relationship in enumerate(RELATIONSHIPS):
            world = _body_world(scenario, affect=affect, relationship=relationship)
            rng = random.Random(4)
            records = world.model_state[BODY]["agents"]
            for name in (
                "positive_affect",
                "negative_affect",
                "reassurance",
                "uncertainty",
                "discontent",
            ):
                records[first][name] = rng.random()
                records[second][name] = rng.random()
            fixture.world(f"body_models.{affect_index}.{relationship_index}", world, None)

    # `test_many_random_events_are_python_s_bit_for_bit`.
    world = _body_world(scenario, affect="trust-weighted-positive-negative-v1")
    people = sorted(world.agents)[:8]
    rng = random.Random(11)
    records = world.model_state[BODY]["agents"]
    for person in people:
        for name in records[person]:
            records[person][name] = rng.random()
    fixture.world("body_random", world, None)
    choices = [item for _ in range(12) for item in _payloads(*rng.sample(people, 2))]
    rng.shuffle(choices)
    events = []
    for kind, payload in choices:
        changed = dict(payload)
        if "distance_m" in changed:
            changed["distance_m"] = rng.choice([0.0, rng.random() * 20, 1e308])
        if "contact_minutes" in changed:
            changed["contact_minutes"] = rng.choice([0, 1, 2.5, 1e9, -3])
        if "interaction_duration_minutes" in changed:
            changed["interaction_duration_minutes"] = rng.choice([0, 1, 90, 0.25, -1, None, "x"])
        events.append([kind, changed])
    inputs["random_body_events"] = events

    # `test_a_positions_distance_reaches_trust_and_the_deltas`.
    world = _body_world(scenario)
    positions = world.model_state["movement_v1"]["positions"]
    positions[first]["point"] = {"x": 1.25, "y": 2.0}
    positions[second]["point"] = {"x": 4.0, "y": 6.5}
    fixture.world("body_positions", world, None)

    # `test_a_correction_is_python_s_bit_for_bit`: without and with the evidence-aware model.
    for evidence_aware in (False, True):
        world = _body_world(scenario, relationship="duration-contact-trust-v1")
        if evidence_aware:
            world.model_configs["policy_registry"]["information"] = "evidence-aware-rumor-v1"
        zone = next(iter(world.agents.values())).zone_id
        for record in world.model_state[BODY]["agents"].values():
            record["uncertainty"] = 0.5
        people = sorted(id_ for id_, agent in world.agents.items() if agent.zone_id == zone)
        information = create_information_runtime_state(set(world.agents))
        for person, last_event_id in ((people[0], "event-1"), (people[-1], "other")):
            information["beliefs"][person]["M"] = AgentInformationState(
                message_id="M",
                belief="verified",
                source="S",
                last_contact=0,
                last_event_id=last_event_id,
                correction_exposed=True,
                correction_accepted=True,
                last_correction_accepted=True,
            ).to_dict()
        world.model_state["information_spread_v1"] = information
        fixture.world(f"body_correction.{int(evidence_aware)}", world, None)
        inputs["expected"][f"correction_actors.{int(evidence_aware)}"] = (
            [[people[0]]] if evidence_aware else [[person] for person in people]
        )
        inputs["expected"]["correction_zone"] = zone


def people_rows_fixture() -> dict[str, Any]:
    """`people_rows.json`."""
    scenario = _scenario()
    fixture = Fixture()
    inputs: dict[str, Any] = {"events": {}, "expected": {}}
    _lifecycle(fixture, inputs, scenario)
    _social(fixture, inputs, scenario)
    _body(fixture, inputs, scenario)
    return {**fixture.document(), "inputs": inputs}

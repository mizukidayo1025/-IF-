"""The offline intervention compiler's answers, for the core's test `interpret.rs` (Issue #303).

`offline_interpreter_fixture` writes `native/core/tests/data/offline_interpreter.json`:
scenario summaries (`scenario_summary_from_world` of real worlds, and synthetic ones with dates,
several zones, water stations, population and zone-environment policies, and none of the
installed primitives), and a corpus of request texts (Japanese and English; Day, 日目 and dates;
午前・午後; kanji numerals; ranges; several clauses; ambiguous and named zones;
unsupported requests; counts out of range; the next morning; contact details the payload
redacts). For each pair it records the sha256 of the
canonical JSON of `compile_natural_language(...).model_dump(mode="json")` without an API key, or
`"invalid"` where Python raises a ValueError (the route's 400). The Rust test compiles each request
with `sheltersim_core::interpret::compile_offline` and compares.
"""

from __future__ import annotations

import hashlib
import json
from datetime import date
from typing import Any

from support.paths import SCENARIOS

TEXTS: tuple[str, ...] = (
    "Day4 14:00にトイレ2基故障",
    "4日目14時にトイレ二基が故障",
    "4日目午後2時に便器1台が壊れた",
    "Day 4 at 2pm toilet 2 fail",
    "Day 4 staff 2 add",
    "4日目にスタッフを2人追加",
    "4日目に職員二名増員",
    "Day4 add 2 toilets",
    "4日目に仮設トイレを2基追加",
    "4日目にトイレが使えなくなる",
    "4日目にトイレ二基の利用を停止",
    "4日目スタッフ二人追加",
    "4日目にスタッフを二人増やす",
    "Day4 water outage",
    "4日目に断水",
    "4日目の給水が停止",
    "Day 4 14:00 toilet_01 outage",
    "Day 1 10:00から12:00まで断水",
    "Day 2 9:00 to 17:00 water outage",
    "Day4 toilet 2 failure and staff 2 added",
    "Day 3 08:00 トイレ1基故障、スタッフを3人追加",
    "2026-10-14 14:00から16:00まで toilet outage",
    "10/13 9am toilet outage",
    "2026-02-30 toilet outage",
    "午後にトイレが故障",
    "翌朝トイレが故障",
    "next morning add 2 staff",
    "Day4 staff -2 add",
    "Day4 staff 1000 add",
    "Day4 toilet 5 fail",
    "トイレを30基追加",
    "感染者を1000人増やして",
    "Day4 toilet missing-restroom failure",
    "zone_id: sleeping スタッフを2人追加",
    "zone: nowhere add 2 staff",
    "避難スペースエリアにスタッフ2人追加",
    "トイレを1基追加",
    "スタッフを2人追加",
    "避難者が10人到着",
    "Day 2 12:00 避難者5人が帰宅",
    "Day 2 10:00から11:00まで避難者3人が到着",
    "evacuees 4 arrive at 13:00",
    "Day4 14:00にトイレ2基故障、さらに雨が降る",
    "Day 3 7:00 toilet outage while the generator is down",
    "Day 4 14:00 18:00 20:00 toilet outage",
    "Day 4 14:00 and 16:00 toilet outage",
    "Day 2 14:00 toilet outage, Day 3 10:00 add 2 staff",
    "Day 400 toilet outage",
    "Day 0 toilet outage",
    "Day ８ 14：00 トイレ故障",
    "Day 2 ２時 トイレ故障",
    "Day 2 ٣:٠٠ toilet outage",
    "Day 2 午前11時30分 トイレ1基故障",
    "Day 2 12am toilet outage",
    "Day 2 24:00 toilet outage",
    "Day 2 13:75 toilet outage",
    "連絡先 test@example.com 090-1234-5678 Day4 トイレ故障",
    "電話 0312345678901 day 2 add 3 staff",
    "Day 5 water station outage from 10:00 to 12:00",
    "Day5 water_station_main outage 10:00-12:00",
    "Day 6 13:00 facility outage",
    "Day 6 13:00 施設が停止",
    "Day 6 toilet-1 broken",
    "Day 6 Restroom 1 broken",
    "Day 7 23:59 add 1 toilet",
    "DAY 3 ADD 20 TOILETS",
    "三十五人のスタッフを追加",
    "十二人のスタッフを追加",
    "二十人の職員を追加",
    "Day 1 add +3 staff",
    "Day 3 install 2 toilets then add 1 staff and water outage 9:00-10:00",
    "トイレ\x1cを2基追加",
    "  Day 2   add 2 staff  ",
    "雨が降る",
    "toilet",
    "Day 2 10:00 zones add 2 staff",
    "Day 1 6:00 evacuees depart 3",
    "Day 1 6:00 remove 3 evacuees",
    "Day 1 10:00 toilet-1 outage and add 2 staff",
    "Day 1 10:00 toilet_01 outage, add 2 staff",
    "Day 1 10:00から12:00まで toilet_01 1基 outage",
    "Day 1 10:00 to 12:00 water outage",
    "Day 1 9:00-11:00 断水、スタッフを3人追加",
    "Day 1 9:00-11:00 断水、トイレ1基故障",
    "Day 1 05:00 避難者が10人到着、スタッフを2人追加",
    "Day 1 05:00 evacuees 2 depart",
)

# (summary, text) pairs whose answers differ only in branch and minute.
MINUTES: tuple[tuple[str, int], ...] = (("child-1", 3000), ("main", 1439))


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def _generated_scenario(**fields: Any) -> Any:
    """The Scenario the Project screen's preview makes of the form with `fields` changed."""
    import copy
    import tempfile
    from pathlib import Path

    from sheltersim.experiments.scenario import ScenarioSpec
    from support.generated import _generated_request, _ui_preview

    with tempfile.TemporaryDirectory(prefix="core-interpret-") as root:
        preview = _ui_preview(Path(root), _generated_request(**fields))
    return ScenarioSpec.model_validate(copy.deepcopy(preview["scenario"]))


def _summaries() -> dict[str, dict[str, Any]]:
    from sheltersim.core.world import WorldState
    from sheltersim.domain.environment import ScenarioZoneEnvironmentConfig
    from sheltersim.experiments.scenario import ScenarioSpec
    from sheltersim.llm.natural_language import scenario_summary_from_world
    from sheltersim.llm.schemas import FacilityTarget, ScenarioSummary, ZoneTarget
    from support.core_fixtures_texts import _generated_world

    def dump(summary: Any) -> dict[str, Any]:
        return summary.model_dump(mode="json")

    parser = ScenarioSummary(
        duration_minutes=10080,
        start_date=date(2026, 10, 12),
        default_zone_id="main",
        facilities=[
            FacilityTarget(
                id=f"toilet-{index}",
                name=f"Restroom {index}",
                kind="toilet",
                zone_id="main",
                capacity=4,
                working_units=4,
            )
            for index in (1, 2)
        ]
        + [
            FacilityTarget(
                id="water-station",
                name="Water station",
                kind="water_station",
                zone_id="main",
                capacity=1,
                working_units=1,
            )
        ],
        zones=[
            ZoneTarget(id="main", name="Main hall", has_water_station=True),
            ZoneTarget(id="sleeping", name="Sleeping area"),
        ],
        available_policies=[
            "facility_capacity",
            "operational_events_v1",
            "resource_service_v1",
            "staff_operations",
        ],
    )
    shelter = ScenarioSummary(
        duration_minutes=1440,
        facilities=[
            FacilityTarget(
                id=facility_id,
                name=facility_id,
                kind=kind,
                zone_id=zone_id,
                capacity=2,
                working_units=1,
            )
            for facility_id, kind, zone_id in (
                ("toilet_01", "toilet", "toilet"),
                ("intake_desk", "intake", "intake"),
                ("water_station_main", "water_station", "food"),
            )
        ],
        zones=[
            ZoneTarget(id="food", name="食事・給水スペース", staff_count=5, evacuee_count=52),
            ZoneTarget(id="intake", name="受付"),
            ZoneTarget(id="sleeping", name="避難スペース", evacuee_count=38),
            ZoneTarget(id="staff", name="運営本部", staff_count=1, evacuee_count=7),
            ZoneTarget(id="toilet", name="トイレ", evacuee_count=3),
        ],
        staff_ids=["s1", "s2"],
        available_staff_ids=["s1"],
        available_policies=[
            "facility_capacity",
            "population_dynamics_v1",
            "staff_operations",
            "zone_environment_v1",
        ],
        zone_environment_settings=ScenarioZoneEnvironmentConfig(),
    )
    lone = ScenarioSummary(
        duration_minutes=4320,
        facilities=[
            FacilityTarget(
                id="tank", name="給水タンク", kind="Water_Station", capacity=1, working_units=1
            )
        ],
        zones=[ZoneTarget(id="hall", name="hall")],
        available_policies=["operational_events_v1", "resource_service_v1"],
    )
    empty = ScenarioSummary(duration_minutes=2880, available_policies=["staff_operations"])
    generated = _generated_world()
    week = WorldState.from_scenario(_generated_scenario(duration_days=7))
    multi = WorldState.from_scenario(ScenarioSpec.load(SCENARIOS / "multi_operational.json"))
    return {
        "parser": dump(parser),
        "shelter": dump(shelter),
        "lone": dump(lone),
        "empty": dump(empty),
        "generated": dump(scenario_summary_from_world(generated)),
        "multi_operational": dump(scenario_summary_from_world(multi)),
        "generated_7d": dump(scenario_summary_from_world(week)),
    }


def _answer(request: dict[str, Any]) -> str:
    from sheltersim.llm.config import LLMMode, LLMSettings
    from sheltersim.llm.natural_language import compile_natural_language
    from sheltersim.llm.provider import LLMRouter

    router = LLMRouter(LLMSettings(mode=LLMMode.OFFLINE))
    try:
        result = compile_natural_language(request, router)
    except ValueError:
        return "invalid"
    return hashlib.sha256(_canonical(result.model_dump(mode="json")).encode()).hexdigest()


# Texts for what the routes around the compiler copy (#306, #308): whether `safe_document` redacts
# a draft's text, and the question a result question stores.
SAFETY_TEXTS: tuple[str, ...] = (
    "Day 4 14:00 toilet_01 outage",
    "連絡先 test@example.com",
    "model@v1.2",
    "a@b.cc",
    "x@ｙ.ｃｏｍ",
    "key sk-ant-abcdefghijklmn here",
    "task-1234567890123 sk-12",
    "Authorization: Bearer abc.def",
    "BEARER",
    "電話 090-1234-5678",
    "+81 90-1234-5678",
    "2026-10-14",
    "03 1234 5678",
    "Day 2 0312345678901 add staff",
    "agent A001 さんの様子",
    "person_42 と staff:7 は? 田中さん、山田様 RUN-12",
    "branch main で ABC123 は",
    "  何が起きた?  ",
    "x\x01y\tz",
    "ﾄｲﾚ ①",
)


def _question_text(question: str) -> str:
    from sheltersim.llm.natural_language import _sanitize_narrative
    from sheltersim.web.results_chat import _REDACT_ENTITY

    return _REDACT_ENTITY.sub("[selected entity]", _sanitize_narrative(question)).strip()


def offline_interpreter_fixture() -> dict[str, Any]:
    summaries = _summaries()
    cases = []
    for name in summaries:
        for text in TEXTS:
            cases.append({"summary": name, "text": text, "branch_id": "main", "minute": 60})
    for branch_id, minute in MINUTES:
        for text in TEXTS[:12]:
            cases.append(
                {"summary": "parser", "text": text, "branch_id": branch_id, "minute": minute}
            )
    # What pydantic refuses: no text, too much text, a minute at the end, an ID too long for the
    # issue that names it, a branch that is not a StableId.
    long_id = "Day 2 toilet " + "x" * 600 + " broken"
    for text, branch_id, minute in (
        ("", "main", 0),
        ("あ" * 4000 + " toilet outage", "main", 0),
        ("あ" * 4001, "main", 0),
        (long_id, "main", 0),
        ("Day 2 add 2 staff", "main", 1440),
        ("Day 2 add 2 staff", "-bad", 0),
    ):
        cases.append({"summary": "shelter", "text": text, "branch_id": branch_id, "minute": minute})
    for case in cases:
        request = {
            "text": case["text"],
            "branch_id": case["branch_id"],
            "selected_minute": case["minute"],
            "scenario_summary": summaries[case["summary"]],
        }
        case["answer"] = _answer(request)
    from sheltersim.storage.projects import safe_document

    safety = [
        {"text": text, "redacts": safe_document(text) != text, "question": _question_text(text)}
        for text in SAFETY_TEXTS
    ]
    return {"summaries": summaries, "cases": cases, "safety": safety}

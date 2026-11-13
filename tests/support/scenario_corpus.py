"""The scenario corpus: what the Python server answers to the Scenario screen, offline (#293).

The browser (WASM core and JS, #285) must answer `scenario-preview`, `scenario-approve` and
`POST runs` as the VM does without an API key. This corpus is the yardstick of the lanes that
port them (#291): for each case, the body the screen posts to `scenario-preview` and what the
server makes of it. A case is a text (the example text, rule edge cases, the screen's merged
two-step request) or the structured form, with edge values (no staff, 0 or 20 accessible toilets,
a 10,000-person capacity blocker, half and two and a half days, pets, nursing, private rooms,
wheelchairs, food and water entered or not) and refusals.

What changes from call to call in Python is pinned or masked:

- the clock: the regional research's `datetime.now` reads `NOW` (`frozen_clock`);
- the public services: the research reads `RecordedPublicData`, the GSI address search's
  recorded answers (`ADDRESS_HITS`) and a small synthetic shelter CSV (`SHELTER_CSV`; the real
  national file is 11.7 MB). No e-Stat key, JMA area or configured regional source is set, as on
  the VM, so nothing else is asked;
- `draft_id` and `preview_id` (uuid4) are left out.

`case_outputs(name)` computes one case in full (bytes included): the preview's answer, the
Scenario's canonical bytes, the FacilityDesign's bytes, the research, the minute-0 world as
`export_world` writes it for the browser (`scripts/build_browser_sample.py`), the Run's
`model_versions` and the evidence route's answer. It also checks that the Scenario is a fixed
point of `ScenarioSpec.model_validate` (the browser builds Scenarios without the full validation,
#285). `corpus_document()` is the committed file, `native/core/tests/data/scenario_corpus.json`:
each case's body and research (by id), the small values in full, and the sha256 and length of the
large ones. `python benchmarks/scenario_corpus.py` writes it again, or a few cases of it, and
`--dump DIR CASE...` writes a case's full bytes for debugging.
`tests/integration/test_scenario_corpus.py` checks each committed case against what the server
makes today.
"""

from __future__ import annotations

import contextlib
import gc
import hashlib
import json
import os
import tempfile
from collections.abc import Iterator
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlsplit

from support.paths import CORE_TEST_DATA

SCHEMA_VERSION = 1
CORPUS_PATH = CORE_TEST_DATA / "scenario_corpus.json"
PROJECT_ID = "corpus"
RUN_ID = "run-corpus"
# The research's clock (research.py, providers.py, additional_providers.py).
NOW = datetime(2026, 11, 1, 3, 0, tzinfo=UTC)
# The example text of the screen's 「例文を使う」 (demo-v2.js; scripts/build_project_sample.py).
EXAMPLE_TEXT = "墨田区で地震が発生。避難者100人、7日間。高齢者や子ども連れの支援ニーズも見たい。"

# ---- The recorded public services ---------------------------------------------------------------

# The GSI address search's answers by query (`q`): (title, longitude, latitude). Recorded in the
# service's shape (a GeoJSON feature list); the points are rounded, not survey positions. A query
# not here answers an empty list ("住所検索から自治体候補を確認できませんでした。").
ADDRESS_HITS: dict[str, list[tuple[str, float, float]]] = {
    "墨田区": [("東京都墨田区", 139.8016, 35.7107)],
    "東京都墨田区": [("東京都墨田区", 139.8016, 35.7107)],
    "横浜市": [("神奈川県横浜市", 139.6380, 35.4437)],
    "札幌市": [("北海道札幌市", 141.3544, 43.0621)],
    "名古屋市": [("愛知県名古屋市", 136.9066, 35.1815)],
    "神戸市": [("兵庫県神戸市", 135.1955, 34.6901)],
    "仙台市": [("宮城県仙台市", 140.8694, 38.2682)],
    "府中市": [("東京都府中市", 139.4776, 35.6689), ("広島県府中市", 133.2366, 34.5683)],
    "合成市": [("合成県合成市", 139.1, 35.1)],
}
# A synthetic register in the GSI national CSV's columns: real municipality codes, invented
# facilities (no real building is described). The research finds candidates by name and rows by
# code (「自治体コード: 13107」).
SHELTER_CSV = (
    "共通ID,都道府県名及び市町村名,施設・場所名,住所,受入対象者,緯度,経度\n"
    "E1310700001111,東京都墨田区,合成第一小学校,東京都墨田区合成一丁目1,,35.71,139.80\n"
    "E1310700002121,東京都墨田区,合成福祉会館,東京都墨田区合成二丁目2,要配慮者,35.72,139.81\n"
    "E1310700003211,東京都墨田区,合成公園,東京都墨田区合成三丁目3,,35.70,139.79\n"
    "E1410000001111,神奈川県横浜市,合成中学校,神奈川県横浜市合成町1,,35.44,139.63\n"
    "E1320600001111,東京都府中市,合成体育館,東京都府中市合成町2,,35.67,139.47\n"
    "E3420800001111,広島県府中市,合成公民館,広島県府中市合成町3,,34.57,133.24\n"
)


class _Response:
    """http.client's response as the providers read it (public_data.py's, without failures)."""

    status = 200

    def __init__(self, body: bytes) -> None:
        from support.public_data import _Response as Fake

        self._fake = Fake(body)
        self.headers = self._fake.headers

    def __enter__(self) -> _Response:
        return self

    def __exit__(self, *_args: object) -> None:
        return None

    def read(self, size: int = -1) -> bytes:
        return self._fake.read(size)

    def read1(self, size: int) -> bytes:
        return self._fake.read1(size)

    def isclosed(self) -> bool:
        return self._fake.isclosed()

    @property
    def fp(self) -> Any:
        return self._fake.fp


class RecordedPublicData:
    """Answers the research's requests from the recordings; refuses any other host."""

    def __init__(self) -> None:
        self.urls: list[str] = []

    def open(self, request: Any, *, timeout: float) -> _Response:
        url = request.full_url
        self.urls.append(url)
        parts = urlsplit(url)
        if parts.hostname == "hinanmap.gsi.go.jp":
            return _Response(SHELTER_CSV.encode("utf-8"))
        if parts.hostname == "msearch.gsi.go.jp":
            query = parse_qs(parts.query).get("q", [""])[0]
            document = [
                {
                    "geometry": {"coordinates": [longitude, latitude], "type": "Point"},
                    "type": "Feature",
                    "properties": {"addressCode": "", "title": title},
                }
                for title, longitude, latitude in ADDRESS_HITS.get(query, [])
            ]
            return _Response(json.dumps(document, ensure_ascii=False).encode("utf-8"))
        raise AssertionError(f"the corpus asked an unrecorded service: {url}")


class _FrozenDatetime(datetime):
    @classmethod
    def now(cls, tz: Any = None) -> datetime:  # type: ignore[override]
        return NOW if tz is not None else NOW.replace(tzinfo=None)


_ENVIRONMENT = {
    "SHELTERSIM_LLM_MODE": "offline",
    "ANTHROPIC_API_KEY": None,
    "ESTAT_APP_ID": None,
    "SHELTERSIM_REGION_DATA_SOURCES_JSON": None,
}


@contextlib.contextmanager
def recorded_world(directory: Path) -> Iterator[RecordedPublicData]:
    """The VM's offline setting with the clock at NOW and the public services recorded; the
    usage ledgers in `directory` (the builder also runs outside pytest)."""
    from sheltersim.region import additional_providers, providers, research

    names = {**_ENVIRONMENT, "SHELTERSIM_PROJECTS_DIR": None, "XDG_STATE_HOME": None}
    saved_env = {name: os.environ.get(name) for name in names}
    values = {
        **_ENVIRONMENT,
        "SHELTERSIM_PROJECTS_DIR": str(directory / "ledgers"),
        "XDG_STATE_HOME": str(directory / "state"),
    }
    for name, value in values.items():
        if value is None:
            os.environ.pop(name, None)
        else:
            os.environ[name] = value
    fake = RecordedPublicData()
    modules = (research, providers, additional_providers)
    saved = [(module, module.datetime) for module in modules]
    saved_openers = [(module, module.build_opener) for module in (providers, additional_providers)]
    for module in modules:
        module.datetime = _FrozenDatetime
    for module, _ in saved_openers:
        module.build_opener = lambda *_args: fake
    providers.clear_region_provider_cache()
    try:
        yield fake
    finally:
        providers.clear_region_provider_cache()
        for module, value in saved:
            module.datetime = value
        for module, value in saved_openers:
            module.build_opener = value
        for name, value in saved_env.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value


# ---- The cases ----------------------------------------------------------------------------------


def form(**fields: Any) -> dict[str, Any]:
    """The structured form as `scenarioRequest()` of demo-v2.js sends it: the given fields, each
    marked as user input, in the screen's order; `facility_type` and `food`/`water_l` as the
    screen turns them into `facility_conditions` and `operational_conditions`."""
    request: dict[str, Any] = {}
    origins: dict[str, str] = {}
    for key in (
        "municipality",
        "disaster",
        "evacuees",
        "duration_days",
        "staff_count",
        "gross_area_m2",
        "accessible_toilet_count",
    ):
        if key in fields:
            request[key] = fields[key]
            origins[key] = "user"
    if "facility_type" in fields:
        request["facility_conditions"] = [f"施設種別（利用者入力）: {fields['facility_type']}"]
        origins["facility_conditions"] = "user"
    if "food" in fields or "water_l" in fields:
        food, water = fields.get("food"), fields.get("water_l")
        request["operational_conditions"] = {
            "initial_resources": {
                "status": "entered" if food is not None and water is not None else "incomplete",
                "food_meal_units": food,
                "water_ml": None if water is None else _js_round(water * 1000),
                "source_kind": "operator",
            }
        }
        origins["operational_conditions"] = "user"
    request["field_origins"] = origins
    return request


def _js_round(value: float) -> int:
    """JavaScript's Math.round (half up)."""
    import math

    return math.floor(value + 0.5)


def _js_number(value: Any) -> str:
    """How a template literal writes a JSON number (`${2.5}`, `${7}`)."""
    if isinstance(value, float) and value.is_integer():
        return str(int(value))
    return str(value)


def screen_hints(fields: dict[str, Any]) -> str:
    """The hint line `previewScenario()` adds to the text when the form lacks a required field."""
    hints = [
        f"{fields['municipality']}で" if fields.get("municipality") else None,
        fields.get("disaster"),
        f"{_js_number(fields['evacuees'])}人" if fields.get("evacuees") else None,
        f"{_js_number(fields['duration_days'])}日間" if fields.get("duration_days") else None,
    ]
    return "、".join(item for item in hints if item)


# Each case: name -> the body of `scenario-preview`, or ("screen", text, form fields): the
# screen's two-step request (`previewScenario()`), whose first call is the case `<name>-hint` and
# whose second posts the text with the merged request.
CASES: dict[str, Any] = {
    # The example text, and texts through the rule parser (region_prepare.py).
    "text-example": {"text": EXAMPLE_TEXT},
    "text-flood-yokohama": {"text": "横浜市で洪水。避難者50人、3日間。"},
    "text-snow-sapporo-names": {"text": "札幌市で大雪、避難者200名、2.5日。冬の夜。"},
    "text-typhoon-hours": {"text": "名古屋市で台風。120人、12時間。"},
    "text-earthquake-weeks": {"text": "神戸市で地震、避難者80人、2週間の想定。"},
    "text-half-day": {"text": "墨田区で地震。避難者40人、0.5日。"},
    "text-tsunami-pets": {"text": "仙台市で津波。避難者30人、1日。ペット同伴の人がいる。"},
    "text-nursing-privacy": {
        "text": "墨田区で地震、避難者60人、3日間。授乳室と個室・プライバシーへの配慮が必要。"
    },
    "text-wheelchair": {"text": "墨田区で地震。避難者25人、2日間。車椅子の利用者がいる。"},
    "text-all-conditions": {
        "text": "墨田区で洪水。避難者90人、4日間。ペット、授乳、個室、車いす対応を確認したい。"
    },
    "text-staff-zero": {"text": "墨田区で地震。避難者20人、1日。0人の職員で運営。"},
    "text-staff-toilets-area": {
        "text": "横浜市で地震。避難者150人、5日間。8人の職員、車いすトイレ2基、延床面積1,200m2。"
    },
    "text-toilets-zero": {"text": "墨田区で地震。避難者30人、1日。トイレ0基。"},
    "text-toilets-twenty": {"text": "墨田区で地震。避難者300人、2日間。20基のトイレ。"},
    "text-water-food": {"text": "墨田区で地震。避難者50人、3日間。飲料水300L、食料600食。"},
    "text-water-only": {"text": "墨田区で地震。避難者50人、3日間。水200リットル。"},
    "text-food-only": {"text": "墨田区で地震。避難者50人、3日間。食料150食。"},
    "text-water-ml": {"text": "墨田区で地震。避難者10人、1日。備蓄水: 5000ml、食料: 30食。"},
    "text-seed-summer": {"text": "墨田区で猛暑。避難者35人、2日間、シード7。夏の午後。"},
    "text-prefecture": {"text": "東京都墨田区で地震が発生。避難者45人、3日間。"},
    "text-english": {"text": "Earthquake in Sumida with 40 people for 2 days."},
    "text-area-capacity": {"text": "墨田区で地震。避難者300人、2日間、延床面積500㎡。"},
    # 10,000 people: the draft exceeds the stored draft's limit (409 draft_capacity_exceeded).
    "text-ten-thousand": {"text": "墨田区で地震。避難者10,000人、7日間。"},
    "text-one-person": {"text": "墨田区で地震。避難者1人、1日。"},
    "text-ambiguous-name": {"text": "府中市で地震。避難者70人、2日間。"},
    "text-unknown-place": {"text": "合成村で地震。避難者15人、1日。"},
    "text-municipality-code": {"text": "墨田区（自治体コード: 13107）で地震。避難者40人、2日間。"},
    "text-landslide": {"text": "墨田区で土砂崩れ。避難者20人、1日。"},
    "text-missing-all": {"text": "避難所の様子を見たい。"},
    "text-missing-evacuees": {"text": "墨田区で地震が発生。"},
    "text-blank": {"text": "   "},
    # The structured form (text left empty).
    "form-minimal": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=100),
    },
    "form-full": {
        "text": "",
        "request": form(
            municipality="墨田区",
            disaster="地震",
            evacuees=100,
            duration_days=7,
            staff_count=6,
            gross_area_m2=900,
            accessible_toilet_count=2,
            facility_type="学校（体育館）",
            food=900,
            water_l=1000,
        ),
    },
    "form-half-day": {
        "text": "",
        "request": form(municipality="横浜市", disaster="洪水", evacuees=50, duration_days=0.5),
    },
    "form-two-and-half-days": {
        "text": "",
        "request": form(municipality="横浜市", disaster="台風", evacuees=50, duration_days=2.5),
    },
    "form-ninety-days": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=12, duration_days=90),
    },
    "form-staff-zero": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=40, staff_count=0),
    },
    "form-staff-hundred": {
        "text": "",
        "request": form(
            municipality="墨田区", disaster="地震", evacuees=400, duration_days=1, staff_count=100
        ),
    },
    "form-toilets-zero": {
        "text": "",
        "request": form(
            municipality="墨田区", disaster="地震", evacuees=40, accessible_toilet_count=0
        ),
    },
    "form-toilets-twenty": {
        "text": "",
        "request": form(
            municipality="墨田区",
            disaster="地震",
            evacuees=200,
            duration_days=1,
            accessible_toilet_count=20,
        ),
    },
    "form-small-area": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=100, gross_area_m2=50),
    },
    "form-large-area": {
        "text": "",
        "request": form(
            municipality="墨田区",
            disaster="洪水",
            evacuees=500,
            duration_days=1,
            gross_area_m2=100000,
        ),
    },
    "form-fractional-area": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=30, gross_area_m2=333.3),
    },
    "form-capacity-blocker": {
        "text": "",
        "request": form(
            municipality="墨田区",
            disaster="地震",
            evacuees=2000,
            duration_days=1,
            gross_area_m2=3000,
        ),
    },
    "form-one-person": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=1, duration_days=1),
    },
    "form-community-hall": {
        "text": "",
        "request": form(
            municipality="札幌市", disaster="地震", evacuees=60, facility_type="公民館"
        ),
    },
    "form-community-facility": {
        "text": "",
        "request": form(
            municipality="仙台市", disaster="津波", evacuees=60, facility_type="地域施設"
        ),
    },
    "form-food-water": {
        "text": "",
        "request": form(
            municipality="墨田区",
            disaster="地震",
            evacuees=50,
            duration_days=3,
            food=450,
            water_l=1.5,
        ),
    },
    "form-food-only": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=50, food=0),
    },
    "form-water-only": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=50, water_l=300),
    },
    "form-unknown-place": {
        "text": "",
        "request": form(municipality="合成村", disaster="大雪", evacuees=20, duration_days=2),
    },
    # The form with text: the screen sends both when every required field is in the form.
    "form-and-text": {
        "text": EXAMPLE_TEXT,
        "request": form(municipality="墨田区", disaster="地震", evacuees=80, staff_count=4),
    },
    # The screen's two-step request: the form lacks a required field, so the screen first reads
    # the text with the form's hints, then posts the merged request.
    "screen-municipality-from-text": (
        "screen",
        "墨田区で避難所を開設。",
        {
            "disaster": "地震",
            "evacuees": 60,
            "duration_days": 2.5,
            "staff_count": 3,
        },
    ),
    "screen-evacuees-from-text": (
        "screen",
        "避難者は75人。授乳への配慮が必要。",
        {
            "municipality": "横浜市",
            "disaster": "洪水",
            "accessible_toilet_count": 0,
        },
    ),
    # Refusals: the request's validation (400) and the route's checks (409).
    "refuse-zero-evacuees": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=0),
    },
    "refuse-toilets-over": {
        "text": "",
        "request": form(
            municipality="墨田区", disaster="地震", evacuees=40, accessible_toilet_count=21
        ),
    },
    "form-shortest-duration": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=40, duration_days=0.001),
    },
    "refuse-duration-under-a-minute": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=40, duration_days=0.0003),
    },
    "refuse-text-not-string": {"text": 5},
    "refuse-unknown-field": {
        "text": "",
        "request": form(municipality="墨田区", disaster="地震", evacuees=10),
        "bogus": 1,
    },
    "refuse-no-text": {"request": form(municipality="墨田区", disaster="地震", evacuees=10)},
    "refuse-text-too-long": {"text": "墨田区で地震。避難者10人。" + "あ" * 4000},
}


def case_names() -> list[str]:
    """Every case, with the first call of each two-step screen case before it."""
    names: list[str] = []
    for name, case in CASES.items():
        if isinstance(case, tuple):
            names.append(f"{name}-hint")
        names.append(name)
    return names


# ---- Running a case -----------------------------------------------------------------------------


def _post(api: Any, parts: list[str], body: dict[str, Any]) -> tuple[int, dict[str, Any]]:
    """The server's status and answer to a POST (web/server.py's do_POST)."""
    from sheltersim.storage.projects import StorageError

    try:
        return 200, api.post(parts, body)
    except StorageError as exc:
        return 409, {"error": str(exc), "code": exc.code}
    except (ValueError, UnicodeDecodeError):
        return 400, {"error": "invalid project request"}


def _body(api: Any, name: str) -> dict[str, Any]:
    """The body the screen posts for the case; a two-step case asks its first call here."""
    base = name.removesuffix("-hint")
    case = CASES[base]
    if not isinstance(case, tuple):
        return case
    _, text, fields = case
    hint = {"text": f"{text}\n入力欄の指定: {screen_hints(fields)}"}
    if name.endswith("-hint"):
        return hint
    status, extracted = _post(api, [PROJECT_ID, "scenario-preview"], hint)
    if status != 200 or not extracted.get("request"):
        raise AssertionError(f"{name}: the screen's first call gave no request: {extracted}")
    request = form(**fields)
    merged = {
        **extracted["request"],
        **request,
        "field_origins": {**extracted["request"]["field_origins"], **request["field_origins"]},
    }
    return {"text": text, "request": merged}


def _bytes(value: Any) -> bytes:
    from sheltersim.storage.projects import canonical

    return canonical(value).encode("utf-8")


def _export_bytes(scenario: Any) -> bytes:
    """The minute-0 world as the browser sample writes it (scripts/build_browser_sample.py)."""
    from sheltersim.core.world_export import export_world
    from support.core_fixtures import run_world

    with tempfile.TemporaryDirectory(
        prefix="scenario-corpus-world-", ignore_cleanup_errors=True
    ) as scratch:
        previous, tempfile.tempdir = tempfile.tempdir, scratch
        try:
            world, controller, engine = run_world(scenario)
            entry = export_world(world, controller)
        finally:
            tempfile.tempdir = previous
        del world, controller, engine
        gc.collect()
    return json.dumps(entry, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()


def _run(api: Any, store: Any, name: str) -> dict[str, Any]:
    from sheltersim.experiments.scenario import ScenarioSpec

    body = _body(api, name)
    status, answer = _post(api, [PROJECT_ID, "scenario-preview"], body)
    outputs: dict[str, Any] = {"body": body, "status": status}
    for key in ("draft_id", "preview_id"):
        answer.pop(key, None)
    outputs["answer"] = answer
    if status != 200 or "scenario" not in answer:
        return outputs
    scenario_bytes = _bytes(answer["scenario"])
    scenario = ScenarioSpec.model_validate(answer["scenario"])
    if scenario.canonical_json().encode("utf-8") != scenario_bytes:
        raise AssertionError(f"{name}: the Scenario is not a fixed point of its validation")
    if hashlib.sha256(scenario_bytes).hexdigest() != answer["approved_hash"]:
        raise AssertionError(f"{name}: approved_hash is not the Scenario's sha256")
    outputs["scenario_bytes"] = scenario_bytes
    outputs["facility_design_bytes"] = _bytes(answer["facility_design"])
    outputs["research_bytes"] = _bytes(answer["research"])
    if not answer["can_start"]:
        return outputs
    saved = store.save_scenario(PROJECT_ID, scenario, approved_hash=answer["approved_hash"])
    run = store.create_run(PROJECT_ID, RUN_ID, saved["scenario_id"], saved["scenario_version"])
    outputs["model_versions"] = run["model_versions"]
    outputs["evidence_bytes"] = _bytes(api.get([PROJECT_ID, "runs", RUN_ID, "evidence"], {}))
    read_back = store.scenario(PROJECT_ID, saved["scenario_id"], saved["scenario_version"])
    outputs["export_bytes"] = _export_bytes(read_back)
    return outputs


def case_outputs(name: str) -> dict[str, Any]:
    """One case in full, computed now: `body`, `status`, the preview's `answer` (without
    draft_id and preview_id), and when it made a Scenario its canonical bytes and the
    FacilityDesign's and research's; when it can start, the Run's `model_versions` and the bytes
    of the evidence route's answer and of the minute-0 world's export."""
    from sheltersim.storage.projects import ProjectStore
    from sheltersim.web.projects import ProjectAPI

    if name not in case_names():
        raise KeyError(f"no such corpus case: {name}")
    with tempfile.TemporaryDirectory(prefix="scenario-corpus-") as scratch:
        root = Path(scratch)
        with recorded_world(root):
            store = ProjectStore(root / "projects")
            store.create_project(PROJECT_ID)
            api = ProjectAPI(store, recover=False)
            try:
                return _run(api, store, name)
            finally:
                api.close()


# ---- The committed corpus -----------------------------------------------------------------------


def _digest(data: bytes) -> dict[str, Any]:
    return {"sha256": hashlib.sha256(data).hexdigest(), "length": len(data)}


def _short(value: Any) -> str:
    return hashlib.sha256(_bytes(value)).hexdigest()[:16]


def scenario_parts(scenario_bytes: bytes) -> dict[str, str]:
    """The first 16 hex digits of the sha256 of each part of the Scenario's canonical JSON (each
    top-level key; `model_configs.<key>` and `provenance.<key>` for each of theirs), so a port of
    one piece can be compared before the whole Scenario is built. A part is taken from the
    canonical document, where zones, facilities, staff and profiles are sorted by id and events
    by (due_minute, priority, sequence, id), and written as `canonical` writes it (sorted keys,
    `,` and `:`, UTF-8)."""
    document = json.loads(scenario_bytes)
    parts: dict[str, str] = {}
    for key, value in document.items():
        if key in ("model_configs", "provenance"):
            for inner, part in value.items():
                parts[f"{key}.{inner}"] = _short(part)
        else:
            parts[key] = _short(value)
    return parts


def expanded_case(outputs: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any] | None]:
    """A case with its small values in full and its large ones as sha256 and length, and its
    research (None if it made no Scenario). `compact` stores many such cases in the file."""
    answer = dict(outputs["answer"])
    case: dict[str, Any] = {"body": outputs["body"], "status": outputs["status"]}
    research = None
    if "scenario_bytes" in outputs:
        research = answer.pop("research")
        case["research"] = "R-" + _short(research)[:12]
        scenario = answer.pop("scenario")
        case["scenario"] = {
            "id": scenario["id"],
            **_digest(outputs["scenario_bytes"]),
            "parts": scenario_parts(outputs["scenario_bytes"]),
        }
        answer.pop("facility_design")
        case["facility_design"] = _digest(outputs["facility_design_bytes"])
    if "model_versions" in outputs:
        case["run"] = {
            "model_versions": outputs["model_versions"],
            "evidence": _digest(outputs["evidence_bytes"]),
            "export": _digest(outputs["export_bytes"]),
        }
    case["answer"] = answer
    return case, research


def _common(values: list[dict[str, Any]]) -> dict[str, Any]:
    """The entries equal in every dict."""
    if not values:
        return {}
    return {
        key: value
        for key, value in values[0].items()
        if all(key in other and other[key] == value for other in values[1:])
    }


def compact(cases: dict[str, dict[str, Any]], research: dict[str, Any]) -> dict[str, Any]:
    """The corpus file's document of expanded cases: the Scenario parts and model_versions
    entries every case shares are written once, under `common`; each case keeps the others."""
    parts = _common([case["scenario"]["parts"] for case in cases.values() if "scenario" in case])
    versions = _common([case["run"]["model_versions"] for case in cases.values() if "run" in case])
    written: dict[str, dict[str, Any]] = {}
    for name, case in cases.items():
        case = json.loads(json.dumps(case))
        if "scenario" in case:
            own = case["scenario"]["parts"]
            case["scenario"]["parts"] = {k: v for k, v in own.items() if k not in parts}
        if "run" in case:
            own = case["run"]["model_versions"]
            case["run"]["model_versions"] = {k: v for k, v in own.items() if k not in versions}
        written[name] = case
    return {
        "schema_version": SCHEMA_VERSION,
        "kind": "scenario_corpus",
        "now": NOW.isoformat(),
        "shelter_csv_sha256": hashlib.sha256(SHELTER_CSV.encode("utf-8")).hexdigest(),
        "common": {"scenario_parts": parts, "model_versions": versions},
        "research": research,
        "cases": written,
    }


def expand(document: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """The cases of a corpus document with the shared entries put back."""
    common = document["common"]
    cases: dict[str, dict[str, Any]] = {}
    for name, case in document["cases"].items():
        case = json.loads(json.dumps(case))
        if "scenario" in case:
            case["scenario"]["parts"] = {**common["scenario_parts"], **case["scenario"]["parts"]}
        if "run" in case:
            case["run"]["model_versions"] = {
                **common["model_versions"],
                **case["run"]["model_versions"],
            }
        cases[name] = case
    return cases


def corpus_document(names: list[str] | None = None) -> dict[str, Any]:
    """The corpus file's document, made now. With `names`, only those cases are made again and
    the others are taken from the committed file (the cases not in CASES any more are dropped)."""
    if names:
        committed = json.loads(CORPUS_PATH.read_text(encoding="utf-8"))
        cases = expand(committed)
        research = dict(committed["research"])
    else:
        cases, research = {}, {}
    for name in names or case_names():
        cases[name], made = expanded_case(case_outputs(name))
        if made is not None:
            research[cases[name]["research"]] = made
    order = case_names()
    cases = {name: cases[name] for name in order if name in cases}
    used = {case["research"] for case in cases.values() if "research" in case}
    return compact(cases, {key: research[key] for key in sorted(used)})


def render(document: dict[str, Any]) -> str:
    """The corpus file's text."""
    return json.dumps(document, sort_keys=True, ensure_ascii=False, indent=1) + "\n"

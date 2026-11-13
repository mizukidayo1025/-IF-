"""The regional research of the browser against Python's, offline (Issue #300).

The browser answers the Scenario preview's regional research in JS (`native/wasm/js/research.mjs`):
the GSI address search asked live, the national shelter CSV read once by the site's build
(`providers.gsi_shelter_index`), the other providers not configured. `research_cases.json`
(`research_cases_fixture`) holds what both sides read and what Python answers:

- `now`: the clock of both (`scenario_corpus.NOW`);
- `indexes`: `gsi_shelter_index` of the corpus's synthetic shelter CSV
  (`scenario_corpus.SHELTER_CSV`) and of `EXTRA_CSV`, which each case names (`csv`);
- `address`: the address search's answer to each query (`ADDRESS_ANSWERS`): the status and body
  of the HTTP response, or `null` for a connection that fails;
- `cases`: each research request (`RegionResearchRequest`'s dump) with Python's research of it
  (`RegionResearchService.research`, the clock frozen and the services recorded as in
  `scenario_corpus.recorded_world`), as canonical JSON, and `browser`: `"same"`, or
  `"unsupported"` where the browser refuses (a municipality code, or a name that resolves to one
  code: both need the shelter rows).

`native/wasm/tests/node_research.mjs` runs `research.mjs` on each case with the same answers and
checks the bytes.
"""

from __future__ import annotations

import hashlib
import json
import tempfile
from pathlib import Path
from typing import Any
from urllib.error import URLError
from urllib.parse import parse_qs, urlsplit

from support.scenario_corpus import ADDRESS_HITS, NOW, SHELTER_CSV, recorded_world


def _hits(query: str) -> str:
    return json.dumps(
        [
            {
                "geometry": {"coordinates": [longitude, latitude], "type": "Point"},
                "type": "Feature",
                "properties": {"addressCode": "", "title": title},
            }
            for title, longitude, latitude in ADDRESS_HITS.get(query, [])
        ],
        ensure_ascii=False,
    )


# The address search's answers: the corpus's recordings, and answers at its edges. A query not
# here answers an empty list.
ADDRESS_ANSWERS: dict[str, dict[str, Any] | None] = {
    **{query: {"status": 200, "body": _hits(query)} for query in ADDRESS_HITS},
    "接続失敗市": None,
    "サーバー失敗市": {"status": 503, "body": "busy"},
    "壊れた市": {"status": 200, "body": "[{"},
    "辞書市": {"status": 200, "body": "{}"},
    "形の違う市": {
        "status": 200,
        "body": json.dumps(
            [
                "not an object",
                {"geometry": [], "properties": {}},
                {"geometry": {"coordinates": [139]}, "properties": {}},
                {"geometry": {"coordinates": [139, 35]}, "properties": {"title": ""}},
                {
                    "geometry": {"coordinates": ["139.5", " 35.25 "]},
                    "properties": {"address": "住所だけ"},
                },
                {"geometry": {"coordinates": [None, 35]}, "properties": {"title": "座標なし"}},
                {"geometry": {"coordinates": ["x", 35]}, "properties": {"title": "読めない"}},
                {"geometry": {"coordinates": [139.25, 35.5, 7]}, "properties": {"title": 12}},
                {"geometry": {"coordinates": [139.1, 35.1]}, "properties": {"title": "九番目"}},
            ],
            ensure_ascii=False,
        ),
    },
}

# A shelter CSV with more names: the corpus's, one name under two codes (an ambiguous exact
# name) and 31 names with a common part (more suggestions than are shown). Invented facilities.
EXTRA_CSV = (
    SHELTER_CSV
    + "E9999900001111,東京都府中市,合成第二会館,東京都府中市合成町9,,35.66,139.46\n"
    + "".join(
        f"E9{index:04d}00001111,合成県第{index}市,合成館{index},合成県第{index}市1,,35.0,139.0\n"
        for index in range(1, 32)
    )
)
CSVS = {"corpus": SHELTER_CSV, "extra": EXTRA_CSV}

# The requests of the cases: (municipality, disaster, season, municipality code), on the corpus's
# CSV, then those on `EXTRA_CSV`.
EXTRA_REQUESTS: tuple[tuple[str, str, str | None, str | None], ...] = (
    ("東京都府中市", "earthquake", None, None),
    ("府中市", "flood", None, None),
    ("合成県", "earthquake", None, None),
    ("合成県第1市", "earthquake", None, None),
)
REQUESTS: tuple[tuple[str, str, str | None, str | None], ...] = (
    ("墨田区", "earthquake", None, None),
    ("墨田区", "地震", None, None),
    ("東京都墨田区", "earthquake", None, None),
    ("合成市", "flood", None, None),
    ("合成村", "earthquake", None, None),
    ("府中市", "earthquake", "summer", None),
    ("横浜市", "flood", None, None),
    ("仙台市", "tsunami", None, None),
    ("名古屋市", "typhoon", None, None),
    ("札幌市", "heavy_snow", "winter", None),
    ("神戸市", "landslide", None, None),
    ("Sumida", "Earthquake", None, None),
    ("墨田区", "storm_surge", None, None),
    ("墨田区", "heatwave", None, None),
    ("墨田区", "Flooding and EARTHQUAKE", None, None),
    ("墨田区", "火山", None, None),
    ("東京都", "earthquake", None, None),
    ("東京", "earthquake", None, None),
    ("区", "earthquake", None, None),
    ("ｓｕｍｉｄａ　区", "earthquake", None, None),
    ("接続失敗市", "earthquake", None, None),
    ("サーバー失敗市", "flood", None, None),
    ("壊れた市", "earthquake", None, None),
    ("辞書市", "earthquake", None, None),
    ("形の違う市", "earthquake", None, None),
    ("墨田区", "earthquake", None, "13107"),
)


class _Answers:
    """The opener the research uses: the shelter CSV, and the address search from
    `ADDRESS_ANSWERS`."""

    def __init__(self, csv_text: str) -> None:
        self.csv_text = csv_text

    def open(self, request: Any, *, timeout: float) -> Any:
        from support.public_data import _Response

        parts = urlsplit(request.full_url)
        if parts.hostname == "hinanmap.gsi.go.jp":
            return _Response(self.csv_text.encode("utf-8"))
        query = parse_qs(parts.query).get("q", [""])[0]
        answer = ADDRESS_ANSWERS.get(query, {"status": 200, "body": "[]"})
        if answer is None:
            raise URLError("connection refused")
        response = _Response(answer["body"].encode("utf-8"))
        response.status = answer["status"]
        return response


def shelter_index(csv_text: str) -> dict[str, Any]:
    """`gsi_shelter_index` of a CSV as the recorded world serves it (at NOW, without HTTP
    metadata)."""
    from sheltersim.region.providers import GsiCsvSnapshot, gsi_shelter_index

    raw = csv_text.encode("utf-8")
    snapshot = GsiCsvSnapshot(
        sha256=hashlib.sha256(raw).hexdigest(), retrieved_at=NOW, text=raw.decode("utf-8-sig")
    )
    return gsi_shelter_index(snapshot)


def _canonical(value: Any) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def research_cases_fixture() -> dict[str, Any]:
    """`research_cases.json`: the inputs both sides read and Python's research of each case."""
    from sheltersim.region import providers
    from sheltersim.region.models import RegionResearchRequest
    from sheltersim.region.research import RegionResearchService

    cases = []
    with tempfile.TemporaryDirectory(prefix="research-cases-") as scratch:
        with recorded_world(Path(scratch)):
            for municipality, disaster, season, code, csv_name in (
                *((*request, "corpus") for request in REQUESTS),
                *((*request, "extra") for request in EXTRA_REQUESTS),
            ):
                answers = _Answers(CSVS[csv_name])
                providers.build_opener = lambda *_args, answers=answers: answers
                request = RegionResearchRequest(
                    municipality_query=municipality,
                    municipality_code=code,
                    disaster=disaster,
                    season=season,
                    selected_municipality_name=municipality,
                )
                providers.clear_region_provider_cache()
                result = RegionResearchService().research(request).model_dump(mode="json")
                needs_rows = code is not None or result["shelter_register"]["status"] in {
                    "matched",
                    "no_published_records",
                    "result_limit_exceeded",
                }
                cases.append(
                    {
                        "request": request.model_dump(mode="json"),
                        "csv": csv_name,
                        "research": _canonical(result),
                        "browser": "unsupported" if needs_rows else "same",
                    }
                )
    return {
        "now": NOW.isoformat().replace("+00:00", "Z"),
        "indexes": {name: shelter_index(text) for name, text in CSVS.items()},
        "address": ADDRESS_ANSWERS,
        "cases": cases,
    }

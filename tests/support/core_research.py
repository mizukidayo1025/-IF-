"""The regional research of the browser against Python's, offline (Issue #300).

The browser answers the Scenario preview's regional research in JS (`native/wasm/js/research.mjs`):
the GSI address search asked live, the national shelter CSV read once by the site's build
(`providers.gsi_shelter_index`, and each municipality code's rows, `providers.gsi_shelter_rows`,
#332), the other providers not configured. `research_cases.json` (`research_cases_fixture`) holds
what both sides read and what Python answers:

- `now`: the clock of both (`scenario_corpus.NOW`);
- `indexes` and `rows`: `gsi_shelter_index` and `gsi_shelter_rows` of the corpus's synthetic
  shelter CSV (`scenario_corpus.SHELTER_CSV`), of `EXTRA_CSV`, `ROWS_CSV` and `CLEAN_CSV`, which
  each case names (`csv`);
- `address`: the address search's answer to each query (`ADDRESS_ANSWERS`): the status and body
  of the HTTP response, or `null` for a connection that fails;
- `cases`: each research request (`RegionResearchRequest`'s dump) with Python's research of it
  (`RegionResearchService.research`, the clock frozen and the services recorded as in
  `scenario_corpus.recorded_world`), as canonical JSON, and `browser`: `"same"`, or
  `"unsupported"` where the browser refuses (`UNSUPPORTED`).

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
# A shelter CSV for the lookups that read rows (#332), with the optional column of the
# municipality's notes. Invented facilities:
# - 13901 (合成県甲市): rows of both subtypes; blank, padded, unreadable and out-of-range
#   coordinates; a blank address; blank and padded notes; a common ID repeated (the CSV allows
#   only the same row again); rows without a municipality name (matched only when the name
#   resolved the code); a blank facility name and an emergency site (both set aside);
# - 13902: two names under one code; 13903 (合成県丙市): 501 shelters, over the limit; 13904
#   (合成県丁市): a name written in full-width and spaced forms (the last one is shown).
ROWS_HEADER = (
    "NO,共通ID,都道府県名及び市町村名,施設・場所名,住所,受入対象者,緯度,経度,"
    "その他市町村長が必要と認める事項\n"
)
ROWS_CSV = (
    ROWS_HEADER
    + "1,E1390100001111,合成県甲市,合成甲小学校,合成県甲市一丁目1,,35.5,139.5,\n"
    + "2,E1390100002121,合成県甲市, 合成甲福祉館 ,,高齢者等,35.6,139.6,  車いす可  \n"
    + "3,E1390100003111,合成県甲市,合成甲公園,合成県甲市三丁目3,  ,,abc,\n"
    + "4,E1390100004111,合成県甲市,合成甲会館,合成県甲市四丁目4,,95,-181,\n"
    + '5,E1390100005111,合成県甲市,合成甲体育館,合成県甲市五丁目5,," 35.25 ",1_39.5,\n'
    + "6,E1390100006111,合成県甲市,合成甲中学校,合成県甲市六丁目6,,nan,inf,\n"
    + "7,E1390100001111,合成県甲市,合成甲小学校,合成県甲市一丁目1,,35.5,139.5,\n"
    + "8,E1390100007111,,名前のない市の施設,,,35.0,139.0,\n"
    + "10,E1390100008111,,市名のない施設,,,35.1,139.1,\n"
    + "11,E1390100009111,合成県甲市,   ,合成県甲市九丁目9,,35.2,139.2,\n"
    + "12,E1390100010201,合成県甲市,合成甲避難場所,,,35.3,139.3,\n"
    + "13,E1390200001111,合成県乙市,合成乙小学校,,,35.0,139.0,\n"
    + "14,E1390200002111,合成県乙町,合成乙中学校,,,35.0,139.0,\n"
    + "".join(
        f"{15 + index},E13903{index:05d}111,合成県丙市,合成丙施設{index},,,35.0,139.0,\n"
        for index in range(501)
    )
    + "516,E1390400001111,合成県丁市,合成丁小学校,,,35.0,139.0,\n"
    + "517,E1390400002111,合成県　丁市,合成丁中学校,,,35.0,139.0,\n"
)
# A shelter CSV without a row the lookup sets aside: its lookups are complete.
CLEAN_CSV = (
    "共通ID,都道府県名及び市町村名,施設・場所名,住所,受入対象者,緯度,経度\n"
    "E1399800001111,合成県戊市,合成戊小学校,合成県戊市1,,35.0,139.0\n"
    "E1399800002121,合成県戊市,合成戊福祉館,合成県戊市2,,35.0,139.0\n"
)
CSVS = {"corpus": SHELTER_CSV, "extra": EXTRA_CSV, "rows": ROWS_CSV, "clean": CLEAN_CSV}

# The requests of the cases: (municipality, disaster, season, municipality code), on the corpus's
# CSV, then those on `EXTRA_CSV`.
EXTRA_REQUESTS: tuple[tuple[str, str, str | None, str | None], ...] = (
    ("東京都府中市", "earthquake", None, None),
    ("府中市", "flood", None, None),
    ("合成県", "earthquake", None, None),
    ("合成県第1市", "earthquake", None, None),
)
# The requests on `ROWS_CSV`.
ROWS_REQUESTS: tuple[tuple[str, str, str | None, str | None], ...] = (
    ("合成県甲市", "earthquake", None, None),
    ("合成県甲市", "flood", None, "13901"),
    ("甲市", "earthquake", None, "13901"),
    ("甲", "earthquake", None, "13901"),
    ("合成県乙市", "earthquake", None, None),
    ("合成県乙市", "earthquake", None, "13902"),
    ("合成県乙", "earthquake", None, "13902"),
    ("合成県甲市", "earthquake", None, "13902"),
    ("合成県丙市", "earthquake", None, None),
    ("合成県丙市", "earthquake", None, "13903"),
    ("合成県丁市", "earthquake", None, None),
    ("合成県 丁市", "earthquake", None, "13904"),
    ("合成県甲市", "earthquake", None, "13999"),
    ("甲", "earthquake", None, "13999"),
    # Python's `\d` takes full-width digits (pydantic's pattern too).
    ("合成県甲市", "earthquake", None, "１３９０１"),
    ("甲", "earthquake", None, "１３９０１"),
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
    ("東京都墨田区", "earthquake", None, "13107"),
    ("東京都墨田区", "earthquake", None, "14100"),
    ("東京都府中市", "earthquake", None, "34208"),
    ("府中市", "earthquake", None, "13206"),
    ("横浜市", "flood", None, "99999"),
    ("神奈川県横浜市", "flood", None, None),
)
# The requests on `CLEAN_CSV`.
CLEAN_REQUESTS: tuple[tuple[str, str, str | None, str | None], ...] = (
    ("合成県戊市", "earthquake", None, None),
    ("戊", "earthquake", None, "13997"),
)
# The cases the browser refuses, by request: none since the rows are on the site (#332).
UNSUPPORTED: frozenset[tuple[str, str, str | None, str | None]] = frozenset()


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
    from sheltersim.region.providers import gsi_shelter_index

    return gsi_shelter_index(_snapshot(csv_text))


def shelter_rows(csv_text: str) -> dict[str, dict[str, Any]]:
    """`gsi_shelter_rows` of a CSV, by municipality code."""
    from sheltersim.region.providers import gsi_shelter_rows

    return gsi_shelter_rows(_snapshot(csv_text))


def _snapshot(csv_text: str) -> Any:
    """The snapshot of a CSV as the recorded world serves it (at NOW, without HTTP metadata)."""
    from sheltersim.region.providers import GsiCsvSnapshot

    raw = csv_text.encode("utf-8")
    return GsiCsvSnapshot(
        sha256=hashlib.sha256(raw).hexdigest(), retrieved_at=NOW, text=raw.decode("utf-8-sig")
    )


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
                *((*request, "rows") for request in ROWS_REQUESTS),
                *((*request, "clean") for request in CLEAN_REQUESTS),
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
                refused = (municipality, disaster, season, code) in UNSUPPORTED
                cases.append(
                    {
                        "request": request.model_dump(mode="json"),
                        "csv": csv_name,
                        "research": _canonical(result),
                        "browser": "unsupported" if refused else "same",
                    }
                )
    return {
        "now": NOW.isoformat().replace("+00:00", "Z"),
        "indexes": {name: shelter_index(text) for name, text in CSVS.items()},
        "rows": {name: shelter_rows(text) for name, text in CSVS.items()},
        "address": ADDRESS_ANSWERS,
        "cases": cases,
    }

"""Public data providers: the JSON cache and the GSI shelter CSV download.

How the regional research combines providers and LLM search is checked in
tests/unit/region/test_research.py.
"""

from __future__ import annotations

import pytest

from sheltersim.region import providers


def test_public_json_cache_strips_app_id_and_accepts_query_pairs(
    monkeypatch,
) -> None:
    class Response:
        status = 200

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        def read(self, _limit):
            return b"[]"

    class Opener:
        def __init__(self) -> None:
            self.urls: list[str] = []

        def open(self, request, *, timeout):
            assert timeout == providers._TIMEOUT_SECONDS
            self.urls.append(request.full_url)
            return Response()

    opener = Opener()
    monkeypatch.setattr(providers, "build_opener", lambda *_args: opener)
    providers.clear_region_provider_cache()
    url = (
        "https://api.e-stat.go.jp/rest/3.0/app/json/getStatsList"
        "?appId=secret-a&searchWord=%E4%BB%99%E5%8F%B0%E5%B8%82"
    )

    assert providers._get_json(url) == []
    assert providers._get_json(url.replace("secret-a", "secret-b")) == []
    assert len(opener.urls) == 1
    assert "appId=secret-a" in opener.urls[0]
    providers.clear_region_provider_cache()


_SUMIDA_SHELTER_CSV = (
    "共通ID,都道府県名及び市町村名,施設・場所名,住所,受入対象者,緯度,経度\n"
    "E1310700001111,東京都墨田区,押上小学校,東京都墨田区押上3-46-17,,35.712,139.819\n"
).encode("utf-8")


class _HttpLikeResponse:
    """Mimic http.client: the body is read in pieces and fp closes once it is all read."""

    status = 200

    def __init__(self, body: bytes, *, socket_timeout_supported: bool = True) -> None:
        self._body = body
        self.headers = {"Content-Length": str(len(body))}
        self.timeouts: list[float] = []
        sock = type("Sock", (), {"settimeout": lambda _self, value: self.timeouts.append(value)})()
        raw = type("Raw", (), {})()
        if socket_timeout_supported:
            raw._sock = sock
        self.fp = type("Fp", (), {"raw": raw})()

    def __enter__(self):
        return self

    def __exit__(self, *_args):
        return None

    def isclosed(self) -> bool:
        return self.fp is None

    def read1(self, size: int) -> bytes:
        chunk, self._body = self._body[:size], self._body[size:]
        if not self._body:
            self.fp = None
        return chunk


def _serve_csv(monkeypatch, response: _HttpLikeResponse) -> None:
    class Opener:
        def open(self, _request, *, timeout):
            assert timeout == providers._TIMEOUT_SECONDS
            return response

    monkeypatch.setattr(providers, "build_opener", lambda *_args: Opener())
    providers.clear_region_provider_cache()


def test_gsi_shelter_csv_download_completes_when_the_response_closes(monkeypatch) -> None:
    # Reading the last bytes closes an http.client response; setting the read timeout
    # on it afterwards failed, so every complete download used to be discarded.
    response = _HttpLikeResponse(_SUMIDA_SHELTER_CSV)
    _serve_csv(monkeypatch, response)

    lookup = providers.GsiShelterProvider().search_municipality("東京都墨田区", "13107")

    assert lookup.status == "matched"
    assert lookup.complete
    assert [row["施設・場所名"] for row in lookup.shelters] == ["押上小学校"]
    assert response.timeouts  # every read while open was bounded
    providers.clear_region_provider_cache()


def test_gsi_shelter_csv_still_rejects_an_open_response_without_a_read_timeout(
    monkeypatch,
) -> None:
    _serve_csv(monkeypatch, _HttpLikeResponse(_SUMIDA_SHELTER_CSV, socket_timeout_supported=False))

    with pytest.raises(providers.RegionProviderError, match="could not be bounded"):
        providers.GsiShelterProvider().search_municipality("東京都墨田区", "13107")
    providers.clear_region_provider_cache()


def test_gsi_shelter_rows_hold_each_code_s_matchable_rows_for_the_browser(monkeypatch) -> None:
    """The site's rows by municipality code (#332): for each code of the index, the rows of an
    overnight-shelter common ID with a facility name, in the CSV's order, repeats kept, with
    whether the municipality name is blank and the columns the research reads (None for a
    column the CSV lacks); each pass of `search_municipality` is made from them."""
    import hashlib

    from support.core_research import NOW, ROWS_CSV, SHELTER_CSV

    def snapshot(text: str) -> providers.GsiCsvSnapshot:
        raw = text.encode("utf-8")
        return providers.GsiCsvSnapshot(
            sha256=hashlib.sha256(raw).hexdigest(), retrieved_at=NOW, text=text
        )

    rows = providers.gsi_shelter_rows(snapshot(ROWS_CSV))
    assert list(rows) == list(providers.gsi_shelter_index(snapshot(ROWS_CSV))["codes"])
    first = rows["13901"]
    assert first["format"] == providers.GSI_SHELTER_ROWS_FORMAT
    assert first["sha256"] == hashlib.sha256(ROWS_CSV.encode("utf-8")).hexdigest()
    assert first["columns"] == ["named", *providers.GSI_SHELTER_ROW_COLUMNS]
    ids = [row[1] for row in first["rows"]]
    # No blank facility name (…0009…) and no emergency site (…0010…); the repeat of …0001… kept.
    assert ids == [
        "E1390100001111",
        "E1390100002121",
        "E1390100003111",
        "E1390100004111",
        "E1390100005111",
        "E1390100006111",
        "E1390100001111",
        "E1390100007111",
        "E1390100008111",
    ]
    assert [row[0] for row in first["rows"]] == [True] * 7 + [False] * 2
    padded = [" 合成甲福祉館 ", "", "35.6", "139.6", "高齢者等", "  車いす可  "]
    assert first["rows"][1][2:] == padded
    assert len(rows["13903"]["rows"]) == 501

    def lookup(name: str, code: str | None) -> list[str]:
        found = providers.GsiShelterProvider().search_municipality(name, municipality_code=code)
        return [row["共通ID"] for row in found.shelters]

    def first_of_each(selected: list[list[object]]) -> list[object]:
        seen: set[object] = set()
        return [row[1] for row in selected if not (row[1] in seen or seen.add(row[1]))]

    monkeypatch.setattr(providers, "_get_gsi_shelter_snapshot", lambda _url: snapshot(ROWS_CSV))
    assert lookup("合成県甲市", None) == first_of_each(first["rows"])
    assert lookup("甲", "13901") == first_of_each([row for row in first["rows"] if row[0]])
    # A CSV without the municipality's notes column.
    corpus = providers.gsi_shelter_rows(snapshot(SHELTER_CSV))
    assert {row[-1] for code in corpus for row in corpus[code]["rows"]} == {None}

"""`parse_scenario_text` without an API key, and the core's port of it (#294).

The browser parses the Scenario screen's text in the core (`sheltersim_core::scenario::text`).
For each text here (the corpus's, #293, and texts at the edges of the rules: Unicode digits and
spaces, `re.IGNORECASE`'s non-ASCII letters, word boundaries, numbers Python's `float` refuses,
fallbacks longer than the prompt's schema allows), the core must give what Python gives with the
offline router: the request's dump, `missing_fields`, `source`, `warnings` and `input_sha256`, or
the same refusal (Python's ValueError, which the server answers with 400).
"""

from __future__ import annotations

import json
from typing import Any

import pytest

TEXTS = [
    # The rules' fields one by one.
    "墨田区で地震が発生。避難者100人、7日間。高齢者や子ども連れの支援ニーズも見たい。",
    "横浜市で地震。避難者150人、5日間。8人の職員、車いすトイレ2基、延床面積1,200m2。",
    "墨田区で地震。避難者30人、1日。トイレ0基。",
    "墨田区で地震。避難者300人、2日間。20基のトイレ。",
    "墨田区で地震。避難者50人、3日間。飲料水300L、食料600食。",
    "墨田区で地震。避難者10人、1日。備蓄水: 5000ml、食料: 30食。",
    "墨田区で地震。避難者50人、3日間。水200リットル。",
    "墨田区で地震。避難者50人、3日間。食料150食。",
    "墨田区で猛暑。避難者35人、2日間、シード7。夏の午後。",
    "札幌市で大雪、避難者200名、2.5日。冬の夜。",
    "名古屋市で台風。120人、12時間。",
    "神戸市で地震、避難者80人、2週間の想定。",
    "墨田区（自治体コード: 13107）で地震。避難者40人、2日間。",
    "東京都墨田区で地震が発生。避難者45人、3日間。",
    "神奈川県横浜市で洪水。避難者60人、2日。",
    "北海道で大雪。避難者10人、1日。",
    "Earthquake in Sumida with 40 people for 2 days.",
    "Flood in New York, 12 evacuees, 3 days",
    "flood IN Springfield. 5 people 1 day",
    "Typhoon in Ōsaka; 7 people 2d",
    "in Tokyo 3 people 2 days earthquake",
    "墨田区で地震。避難者20人、1日。0人の職員で運営。",
    "仙台市で津波。避難者30人、1日。ペット同伴の人がいる。",
    "墨田区で洪水。避難者90人、4日間。ペット、授乳、個室、車いす対応を確認したい。",
    "合成村で地震。避難者15人、1日。",
    "避難所の様子を見たい。",
    "墨田区で地震が発生。",
    "墨田区で地震。避難者10,000人、7日間。",
    # Python's Unicode classes and IGNORECASE.
    "墨田区で地震。自治体コード：１３１０７。避難者40人、2日間。",
    "墨田区で地震。自治体コード 131070。避難者40人、2日間。",
    "墨田区で地震。市町村コード=13107５。避難者40人、2日間。",
    "墨田区で地震。避難者40　人、2\x1c日間。",
    "墨田区で地震。避難者40\x1d\x1e人、2 日間、シード 9。",
    "墨田区で地震。40 PEOPLE, 2 DAYS, SEED: 11",
    "墨田区で地震。40 ſeed 3 people, 2 dayſ",
    "墨田区で地震。40 people, water stocK 30 L, food 20 meal units, 2 days",
    "Earthquake ın Kobe. 40 people 2 days",
    "Earthquake İN Kobe. 40 people 2 days",
    "東京in Tokyo flood 3 people 1 day",
    "flood _in Osaka 3 people 1 day",
    "flood in  Ab 3 people 1 day",
    "flood in A, 3 people 1 day",
    "flood in Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa 3 people 1 day",
    "flood in New Orleans during 3 people 1 day",
    "flood in Kyoto\n",
    "flood in Kyoto\n\n",
    # Numbers: hours and weeks, decimals, sizes, what float() refuses.
    "墨田区で地震。避難者40人、1時間。",
    "墨田区で地震。避難者40人、36 hours。",
    "墨田区で地震。避難者40人、3 weeks。",
    "墨田区で地震。避難者40人、1.5.5日。",
    "墨田区で地震。避難者40人、2日、延床面積1.2.3㎡。",
    "墨田区で地震。避難者40人、2日、面積1,234.5 平方メートル。",
    "墨田区で地震。避難者40人、2日、面積12345678901234567m2。",
    "墨田区で地震。避難者40人、2日、水 1.5 L、食料 1,000 食分。",
    "墨田区で地震。避難者40人、2日、水の在庫 2.5ℓ。",
    "墨田区で地震。避難者40人、2日、在庫水=1000mL。",
    "墨田区で地震。避難者40人、2日、水0.0005L。",
    "墨田区で地震。避難者40人、2日、水0.0015L。",
    "墨田区で地震。避難者0人、2日。",
    "墨田区で地震。避難者0040人、2日。",
    "墨田区で地震。避難者1234567890123人、2日。",
    "墨田区で地震。避難者40人、0日。",
    "墨田区で地震。避難者40人、2日。トイレ 12345 基。",
    "墨田区で地震。避難者40人、2日。123456789012345.5日",
    "墨田区で地震。避難者40人、2日間、seed 99999999999999999999。",
    "墨田区で地震。避難者40人、春、秋。Spring.",
    "STORM_SURGE in Chiba 3 people 1 day",
    "高潮と津波。千葉市。避難者5人。",
    "Wheelchair users, PRIVACY, an Infant and a Pet. 墨田区で地震。避難者5人。",
    "県県県県県県県市で地震。避難者5人。",
    "あいうえおかきくけこさしすせそたちつてとなにぬねのはひふへほまみむめも市で地震。避難者5人。",
    "大阪府大阪市北区で地震。避難者5人。",
    "京都府で地震。避難者5人。",
    # Refusals.
    "",
    "   　\x1c",
    "あ" * 4001,
    "墨田区で地震。避難者40人、2日。" + "あ" * 3980,
]


def _python(text: str) -> tuple[str, Any]:
    """What the server's parse gives offline: ("ok", the fields) or ("error", the class)."""
    from sheltersim.llm.provider import LLMRouter
    from sheltersim.llm.region_prepare import parse_scenario_text

    try:
        parsed = parse_scenario_text(text, router=LLMRouter())
    except (ValueError, OverflowError) as error:
        return "error", "ValueError" if isinstance(error, ValueError) else type(error).__name__
    request = None if parsed.request is None else parsed.request.model_dump(mode="json")
    if request is not None:
        request["field_origins"] = dict(sorted(request["field_origins"].items()))
    return "ok", {
        "request": request,
        "missing_fields": list(parsed.missing_fields),
        "source": parsed.source,
        "warnings": list(parsed.warnings),
        "input_sha256": parsed.input_sha256,
    }


def _core(text: str) -> tuple[str, Any]:
    import sheltersim_native

    from sheltersim.native import Unsupported

    try:
        parsed = json.loads(sheltersim_native.scenario_text_parse(text))
    except Unsupported:
        return "unsupported", None
    except ValueError:
        return "error", "ValueError"
    if parsed["request"] is not None:
        origins = parsed["request"]["field_origins"]
        parsed["request"]["field_origins"] = dict(sorted(origins.items()))
    return "ok", parsed


def _corpus_texts() -> list[str]:
    """The texts the corpus's cases post (#293)."""
    from support.scenario_corpus import CORPUS_PATH, expand

    cases = expand(json.loads(CORPUS_PATH.read_text(encoding="utf-8")))
    texts: list[str] = []
    for case in cases.values():
        text = case.get("body", {}).get("text")
        if isinstance(text, str) and text not in texts:
            texts.append(text)
    return texts


@pytest.mark.parametrize("text", TEXTS, ids=range(len(TEXTS)))
def test_the_core_parses_a_scenario_text_as_python_does_offline(text: str) -> None:
    python = _python(text)
    core = _core(text)

    if core[0] == "unsupported":
        # The request contract does not compute it (#295); Python accepts it.
        assert python[0] == "ok"
        return
    assert core == python


def test_the_core_parses_the_corpus_texts_as_python_does() -> None:
    texts = _corpus_texts()
    assert len(texts) >= 30
    for text in texts:
        assert _core(text) == _python(text), text


def test_the_regex_classes_table_is_what_its_tool_writes() -> None:
    """`native/core/src/text/classes.rs` holds `re`'s `\\d`, `\\s` and `\\w` of this Python."""
    import importlib.util

    from support.paths import CORE_TEST_DATA

    tool = CORE_TEST_DATA.parents[1] / "tools" / "regex_classes_table.py"
    spec = importlib.util.spec_from_file_location("regex_classes_table", tool)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    assert module.TABLE.read_text(encoding="utf-8") == module.text()

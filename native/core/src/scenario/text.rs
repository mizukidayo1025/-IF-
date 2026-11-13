//! `parse_scenario_text` of `llm/region_prepare.py` without an API key (Issue #294): the values a
//! Scenario text states, found by the rules of `_parse_explicit_fields`, and what the offline
//! router makes of them (the rules' own fallback, read back through the prompt's contract and
//! merged), as the request the Scenario screen previews.
//!
//! Each of the rules' regular expressions is matched here by hand, as Python's `re` matches it:
//! the leftmost match, with the backtracking each pattern can need. `\d`, `\s` and `\w` are
//! Python's Unicode classes (`text::re_decimal`, `re_space`, `re_word`); `[0-9]` is ASCII. With
//! `re.IGNORECASE`, an ASCII letter also matches its other case and, as in Python, `i` matches
//! `İ` (U+0130) and `ı` (U+0131), `k` matches the Kelvin sign (U+212A) and `s` matches `ſ`
//! (U+017F), in a literal and in `[A-Za-z]` alike. `str.casefold` is the core's
//! (`text::casefold`).
//!
//! Offline, the router answers the prompt with the rules' fallback (`_to_llm_contract` of the
//! values) after checking it against the prompt's schema; `_parse_llm_contract` reads it back and
//! `_merge_explicit` adds what the rules did not set (the default operational conditions,
//! population conditions and evacuation selection, as `llm_extracted`). A fallback whose text is
//! longer than the schema allows (a duration like `0.041666666666666664`, more than 12 digits of
//! people) fails the check: Python then keeps the rules' values alone, with the source `rules`.

use std::sync::Arc;

use indexmap::IndexMap;

use super::request::{InitialScenarioRequest, Refusal, OPERATOR_CONDITION_SOURCE_ID};
use crate::json::python_float_repr;
use crate::text::{casefold, re_decimal, re_space, re_word};
use crate::value::V;

/// `_MAX_INPUT_LENGTH`, in code points.
const MAX_INPUT_LENGTH: usize = 4_000;
/// The warning of a parse whose AI step fell back to the rules.
pub const FALLBACK_WARNING: &str = "AIの入力整理を利用できず、明示された値のみを使用します。";
/// The warning of every parse with a router.
pub const DRAFT_WARNING: &str = "生成された条件は下書きです。地域・人数・期間・設備を確認してから確定してください。";
/// The fields a request needs, in their order.
const REQUIRED: [&str; 3] = ["municipality", "disaster", "evacuees"];

/// `_DISASTER_TERMS`, with the canonical name first.
const DISASTER_TERMS: [(&str, &[&str]); 8] = [
    ("flood", &["洪水", "水害", "浸水", "氾濫"]),
    ("earthquake", &["地震", "震災"]),
    ("tsunami", &["津波"]),
    ("landslide", &["土砂災害", "土砂崩れ", "がけ崩れ"]),
    ("storm_surge", &["高潮"]),
    ("typhoon", &["台風"]),
    ("heavy_snow", &["大雪", "豪雪"]),
    ("heatwave", &["猛暑", "熱波"]),
];
const SEASONS: [(&str, &[&str]); 4] = [
    ("spring", &["春", "spring"]),
    ("summer", &["夏", "summer"]),
    ("autumn", &["秋", "autumn", "fall"]),
    ("winter", &["冬", "winter"]),
];
const CONDITIONS: [(&str, &[&str]); 4] = [
    ("ペット", &["ペット", "pet"]),
    ("授乳・乳幼児", &["授乳", "乳幼児", "infant"]),
    ("個室・プライバシー", &["個室", "プライバシー", "privacy"]),
    ("車いす対応", &["車椅子", "車いす", "wheelchair", "バリアフリー"]),
];

/// What `parse_scenario_text(text, router=offline router)` gives.
#[derive(Clone, Debug, PartialEq)]
pub struct TextParse {
    /// None while a required field is missing.
    pub request: Option<InitialScenarioRequest>,
    pub missing_fields: Vec<&'static str>,
    /// `rules_fallback`, or `rules` when the fallback failed the prompt's schema.
    pub source: &'static str,
    pub warnings: Vec<&'static str>,
    pub input_sha256: String,
}

impl TextParse {
    /// The parse as JSON values: `request` (its `model_dump(mode="json")`, or null),
    /// `missing_fields`, `source`, `warnings` and `input_sha256`.
    pub fn to_value(&self) -> V {
        let texts = |items: &[&'static str]| V::List(items.iter().map(|item| V::Str(Arc::from(*item))).collect());
        V::Map(
            [
                ("request", self.request.as_ref().map_or(V::Null, InitialScenarioRequest::dump)),
                ("missing_fields", texts(&self.missing_fields)),
                ("source", V::Str(Arc::from(self.source))),
                ("warnings", texts(&self.warnings)),
                ("input_sha256", V::Str(Arc::from(self.input_sha256.as_str()))),
            ]
            .into_iter()
            .map(|(key, value)| (Arc::from(key), value))
            .collect(),
        )
    }
}

// ---- Matching as Python's `re` does ----------------------------------------------------------------

/// The text as code points, which Python's `re` indexes.
struct Text {
    chars: Vec<char>,
}

/// `re.IGNORECASE` of an ASCII letter (lower case) against a character.
fn ci(char: char, letter: char) -> bool {
    char.to_ascii_lowercase() == letter && char.is_ascii()
        || match letter {
            'i' => matches!(char, '\u{130}' | '\u{131}'),
            'k' => char == '\u{212a}',
            's' => char == '\u{17f}',
            _ => false,
        }
}

/// `[A-Za-z]` with `re.IGNORECASE`.
fn ci_letter(char: char) -> bool {
    char.is_ascii_alphabetic() || matches!(char, '\u{130}' | '\u{131}' | '\u{17f}' | '\u{212a}')
}

impl Text {
    fn at(&self, index: usize) -> Option<char> {
        self.chars.get(index).copied()
    }

    /// The end of `word` at `index`, matched exactly.
    fn literal(&self, index: usize, word: &str) -> Option<usize> {
        let mut at = index;
        for char in word.chars() {
            if self.at(at)? != char {
                return None;
            }
            at += 1;
        }
        Some(at)
    }

    /// The end of `word` (ASCII, lower case, maybe with spaces) at `index`, with `IGNORECASE`.
    fn ci_literal(&self, index: usize, word: &str) -> Option<usize> {
        let mut at = index;
        for letter in word.chars() {
            let char = self.at(at)?;
            let fits = if letter.is_ascii_alphabetic() { ci(char, letter) } else { char == letter };
            if !fits {
                return None;
            }
            at += 1;
        }
        Some(at)
    }

    /// The end of the first of these alternatives that matches at `index`.
    fn any_of(&self, index: usize, words: &[&str], ignore_case: bool) -> Option<usize> {
        words.iter().find_map(|word| if ignore_case { self.ci_literal(index, word) } else { self.literal(index, word) })
    }

    /// The end of the longest run of characters of the class from `index` (`class*`).
    fn run(&self, index: usize, class: impl Fn(char) -> bool) -> usize {
        let mut at = index;
        while self.at(at).is_some_and(&class) {
            at += 1;
        }
        at
    }

    /// `\s*`.
    fn spaces(&self, index: usize) -> usize {
        self.run(index, re_space)
    }

    fn slice(&self, start: usize, end: usize) -> String {
        self.chars[start..end].iter().collect()
    }

    /// The first match of `attempt` at a start position, in order: `re.search`.
    fn search<T>(&self, mut attempt: impl FnMut(usize) -> Option<T>) -> Option<T> {
        (0..=self.chars.len()).find_map(|start| attempt(start))
    }

    /// `([0-9]+)` or another number at `start` (`first` for its first character, `rest` for the
    /// others), followed by `tail`: the number's text when `tail` matches after it. The number
    /// takes every character of its class: a shorter one would leave one of them where `tail`
    /// must start, and no `tail` here starts with one.
    fn number_then<T>(&self, start: usize, rest: impl Fn(char) -> bool, tail: impl Fn(usize) -> Option<T>) -> Option<(String, T)> {
        if !self.at(start)?.is_ascii_digit() {
            return None;
        }
        let end = self.run(start + 1, rest);
        tail(end).map(|found| (self.slice(start, end), found))
    }
}

/// `[0-9]+(?:\.[0-9]+)?` at `start`, then `\s*` and a unit `tail`: the number's text. The fraction
/// is tried first, then without it.
fn decimal_then(text: &Text, start: usize, tail: impl Fn(usize) -> Option<usize>) -> Option<String> {
    if !text.at(start)?.is_ascii_digit() {
        return None;
    }
    let whole = text.run(start, |char| char.is_ascii_digit());
    if text.at(whole) == Some('.') && text.at(whole + 1).is_some_and(|char| char.is_ascii_digit()) {
        let end = text.run(whole + 1, |char| char.is_ascii_digit());
        if tail(text.spaces(end)).is_some() {
            return Some(text.slice(start, end));
        }
    }
    tail(text.spaces(whole)).map(|_| text.slice(start, whole))
}

// ---- The rules (`_parse_explicit_fields`) ------------------------------------------------------------

/// A field's value as the rules found it, in the order Python sets them.
type Values = IndexMap<&'static str, V>;

/// Python's `int` of ASCII digits: an i64, or the digits without leading zeros past it.
fn int_value(digits: &str) -> V {
    match digits.parse::<i64>() {
        Ok(number) => V::Int(number),
        Err(_) => {
            let trimmed = digits.trim_start_matches('0');
            V::Big(if trimmed.is_empty() { "0" } else { trimmed }.into())
        }
    }
}

/// Python's `float` of the text the rules took: digits, commas taken out, at most one point.
fn float_value(text: &str) -> Result<f64, Refusal> {
    text.parse::<f64>().map_err(|_| Refusal::Invalid(format!("could not convert string to float: {text:?}")))
}

/// `int(round(value))` of a quantity, which Python computes for any finite size.
fn rounded(value: f64) -> Result<V, Refusal> {
    if !value.is_finite() {
        return Err(Refusal::Invalid("cannot convert float infinity to integer".into()));
    }
    let rounded = value.round_ties_even();
    if rounded.abs() >= 9.2e18 {
        return Err(Refusal::Unsupported("a quantity too large to compute".into()));
    }
    Ok(V::Int(rounded as i64))
}

fn municipality_code(text: &Text) -> Option<String> {
    text.search(|start| {
        let after = text.any_of(start, &["自治体", "市町村"], false)?;
        let mut at = text.literal(after, "コード")?;
        at = text.spaces(at);
        if text.at(at).is_some_and(|char| matches!(char, ':' | '：' | '=')) {
            at = text.spaces(at + 1);
        }
        let end = at + 5;
        if (at..end).all(|index| text.at(index).is_some_and(re_decimal)) && !text.at(end).is_some_and(re_decimal) {
            Some(text.slice(at, end))
        } else {
            None
        }
    })
}

/// `_extract_municipality`.
fn municipality(text: &Text) -> Option<String> {
    let kana_kanji = |char: char| matches!(char, '一'..='龥' | 'ぁ'..='ん' | 'ァ'..='ヴ' | 'ー');
    let with_digits = |char: char| kana_kanji(char) || char.is_ascii_digit();
    let ends = |char: Option<char>| char.is_some_and(|char| matches!(char, '市' | '区' | '町' | '村'));
    // (?:北海道|東京都|京都府|大阪府|[一-龥ぁ-んァ-ヴー]{2,4}県)(?:[一-龥ぁ-んァ-ヴー0-9]+(?:市|区|町|村))?
    let region = text.search(|start| {
        let prefecture = text.any_of(start, &["北海道", "東京都", "京都府", "大阪府"], false).or_else(|| {
            let longest = text.run(start, kana_kanji).min(start + 4);
            (start + 2..=longest).rev().find(|end| text.at(*end) == Some('県')).map(|end| end + 1)
        })?;
        let run = text.run(prefecture, with_digits);
        let city = (prefecture + 1..run).rev().find(|end| ends(text.at(*end))).map(|end| end + 1);
        Some(text.slice(start, city.unwrap_or(prefecture)))
    });
    if region.is_some() {
        return region;
    }
    // [一-龥ぁ-んァ-ヴー0-9]{2,24}(?:市|区|町|村)
    let city = text.search(|start| {
        let longest = text.run(start, with_digits).min(start + 24);
        (start + 2..=longest).rev().find(|end| ends(text.at(*end))).map(|end| text.slice(start, end + 1))
    });
    if city.is_some() {
        return city;
    }
    english_place(text)
}

/// `\bin\s+([A-Za-z][A-Za-z .'-]{1,40}?)(?=\s+(?:for|during|with|disaster|flood|earthquake)|[,.;]|$)`
/// with `re.IGNORECASE`, stripped of ` .,'`.
fn english_place(text: &Text) -> Option<String> {
    let length = text.chars.len();
    let lookahead = |at: usize| {
        let spaces = text.spaces(at);
        (spaces > at && text.any_of(spaces, &["for", "during", "with", "disaster", "flood", "earthquake"], true).is_some())
            || text.at(at).is_some_and(|char| matches!(char, ',' | '.' | ';'))
            || at == length
            || (at + 1 == length && text.at(at) == Some('\n'))
    };
    let body = |char: char| ci_letter(char) || matches!(char, ' ' | '.' | '\'' | '-');
    let found = text.search(|start| {
        let first = text.at(start)?;
        let before = start.checked_sub(1).and_then(|index| text.at(index));
        if re_word(first) == before.is_some_and(re_word) {
            return None;
        }
        let after = text.ci_literal(start, "in")?;
        let name = text.spaces(after);
        if name == after || !text.at(name).is_some_and(ci_letter) {
            return None;
        }
        for count in 1..=40 {
            if !text.at(name + count).is_some_and(body) {
                return None;
            }
            if lookahead(name + count + 1) {
                return Some(text.slice(name, name + count + 1));
            }
        }
        None
    })?;
    Some(found.trim_matches(|char| matches!(char, ' ' | '.' | ',' | '\'')).to_owned())
}

/// `_parse_explicit_fields`.
fn explicit_fields(text: &Text, folded: &str) -> Result<Values, Refusal> {
    let mut values = Values::new();
    if let Some(code) = municipality_code(text) {
        values.insert("municipality_code", V::Str(Arc::from(code.as_str())));
    }
    // ([0-9][0-9,]*)\s*(?:人|名|people|evacuees)
    let people = text.search(|start| {
        text.number_then(start, |char| char.is_ascii_digit() || char == ',', |end| {
            let at = text.spaces(end);
            text.any_of(at, &["人", "名"], false).or_else(|| text.any_of(at, &["people", "evacuees"], true))
        })
    });
    if let Some((digits, _)) = people {
        values.insert("evacuees", int_value(&digits.replace(',', "")));
    }
    // ([0-9]+(?:\.[0-9]+)?)\s*(?:日間|日|days?|d), else hours, else weeks.
    let unit = |first: &'static str, letter: char| move |at: usize| text.literal(at, first).or_else(|| text.at(at).filter(|char| ci(*char, letter)).map(|_| at + 1));
    let days = text.search(|start| decimal_then(text, start, unit("日", 'd')));
    if let Some(days) = days {
        values.insert("duration_days", V::Float(float_value(&days)?));
    } else {
        let hours = text.search(|start| decimal_then(text, start, unit("時間", 'h')));
        let weeks = text.search(|start| decimal_then(text, start, unit("週間", 'w')));
        if let Some(hours) = hours {
            values.insert("duration_days", V::Float(float_value(&hours)? / 24.0));
        } else if let Some(weeks) = weeks {
            values.insert("duration_days", V::Float(float_value(&weeks)? * 7.0));
        }
    }
    // ([0-9][0-9,.]*)\s*(?:m2|m²|㎡|平方メートル)
    let area = text.search(|start| {
        text.number_then(start, |char| char.is_ascii_digit() || char == ',' || char == '.', |end| {
            let at = text.spaces(end);
            text.any_of(at, &["m2", "m²"], true).or_else(|| text.any_of(at, &["㎡", "平方メートル"], false))
        })
    });
    if let Some((area, _)) = area {
        values.insert("gross_area_m2", V::Float(float_value(&area.replace(',', ""))?));
    }
    // ([0-9]+)\s*(?:基|台)\s*(?:の)?\s*(?:車いす|車椅子|バリアフリー)?\s*(?:トイレ|便所), else the
    // toilet before the count.
    let kinds = ["車いす", "車椅子", "バリアフリー"];
    let toilet = |at: usize| text.any_of(at, &["トイレ", "便所"], false);
    let counted = text.search(|start| {
        text.number_then(start, |char| char.is_ascii_digit(), |end| {
            let mut at = text.any_of(text.spaces(end), &["基", "台"], false)?;
            at = text.spaces(at);
            at = text.spaces(text.literal(at, "の").unwrap_or(at));
            at = text.spaces(text.any_of(at, &kinds, false).unwrap_or(at));
            toilet(at)
        })
    });
    let accessible = counted.or_else(|| {
        text.search(|start| {
            let mut at = text.any_of(start, &kinds, false).unwrap_or(start);
            at = toilet(text.spaces(at))?;
            at = text.spaces(at);
            text.number_then(at, |char| char.is_ascii_digit(), |end| text.any_of(text.spaces(end), &["基", "台"], false))
        })
    });
    if let Some((count, _)) = accessible {
        values.insert("accessible_toilet_count", int_value(&count));
    }
    // ([0-9]+)\s*(?:人|名)\s*(?:の)?\s*(?:職員|スタッフ)
    let staff = text.search(|start| {
        text.number_then(start, |char| char.is_ascii_digit(), |end| {
            let at = text.spaces(text.any_of(text.spaces(end), &["人", "名"], false)?);
            let at = text.spaces(text.literal(at, "の").unwrap_or(at));
            text.any_of(at, &["職員", "スタッフ"], false)
        })
    });
    if let Some((count, _)) = staff {
        values.insert("staff_count", int_value(&count));
    }
    // (?:飲料水|備蓄水|水(?:の在庫|在庫)?|water(?: stock)?)\s*[:：=]?\s*([0-9][0-9,.]*)\s*(ml|mL|リットル|ℓ|L|l)
    let colon = |at: usize, marks: &[char]| {
        let at = text.spaces(at);
        let at = if text.at(at).is_some_and(|char| marks.contains(&char)) { at + 1 } else { at };
        text.spaces(at)
    };
    let water = text.search(|start| {
        let named = text
            .any_of(start, &["飲料水", "備蓄水"], false)
            .or_else(|| text.literal(start, "水").map(|at| text.any_of(at, &["の在庫", "在庫"], false).unwrap_or(at)))
            .or_else(|| text.ci_literal(start, "water").map(|at| text.ci_literal(at, " stock").unwrap_or(at)))?;
        let at = colon(named, &[':', '：', '=']);
        text.number_then(at, |char| char.is_ascii_digit() || char == ',' || char == '.', |end| {
            let at = text.spaces(end);
            if text.ci_literal(at, "ml").is_some() {
                return Some(false);
            }
            let liters = text.any_of(at, &["リットル", "ℓ"], false).is_some() || text.at(at).is_some_and(|char| ci(char, 'l'));
            liters.then_some(true)
        })
    });
    // (?:食料|食糧|食事在庫|food(?: stock)?|meal(?: stock)?)\s*[:：=]?\s*([0-9][0-9,]*)\s*(?:食分|食|meal units?)
    let food = text.search(|start| {
        let named = text
            .any_of(start, &["食料", "食糧", "食事在庫"], false)
            .or_else(|| text.ci_literal(start, "food").map(|at| text.ci_literal(at, " stock").unwrap_or(at)))
            .or_else(|| text.ci_literal(start, "meal").map(|at| text.ci_literal(at, " stock").unwrap_or(at)))?;
        let at = colon(named, &[':', '：', '=']);
        text.number_then(at, |char| char.is_ascii_digit() || char == ',', |end| {
            let at = text.spaces(end);
            text.literal(at, "食").or_else(|| text.ci_literal(at, "meal unit"))
        })
    });
    let water_ml = match &water {
        Some((amount, liters)) => Some(rounded(float_value(&amount.replace(',', ""))? * if *liters { 1_000.0 } else { 1.0 })?),
        None => None,
    };
    let food_meal_units = food.as_ref().map(|(digits, _)| int_value(&digits.replace(',', "")));
    if water.is_some() || food.is_some() {
        let status = if water.is_some() && food.is_some() { "entered" } else { "incomplete" };
        let resources = V::Map(
            [
                ("status", V::Str(Arc::from(status))),
                ("water_ml", water_ml.unwrap_or(V::Null)),
                ("food_meal_units", food_meal_units.unwrap_or(V::Null)),
                ("source_ids", V::List(vec![V::Str(Arc::from(OPERATOR_CONDITION_SOURCE_ID))])),
            ]
            .into_iter()
            .map(|(key, value)| (Arc::from(key), value))
            .collect(),
        );
        values.insert("operational_conditions", V::Map([(Arc::from("initial_resources"), resources)].into_iter().collect()));
    }
    // (?:seed|シード)\s*[:=]?\s*([0-9]+)
    let seed = text.search(|start| {
        let named = text.ci_literal(start, "seed").or_else(|| text.literal(start, "シード"))?;
        let at = colon(named, &[':', '=']);
        text.number_then(at, |char| char.is_ascii_digit(), Some)
    });
    if let Some((seed, _)) = seed {
        values.insert("seed", int_value(&seed));
    }
    if let Some((season, _)) = SEASONS.iter().find(|(_, terms)| terms.iter().any(|term| folded.contains(&*casefold(term)))) {
        values.insert("season", V::Str(Arc::from(*season)));
    }
    if let Some((disaster, _)) = DISASTER_TERMS.iter().find(|(name, terms)| folded.contains(name) || terms.iter().any(|term| folded.contains(term))) {
        values.insert("disaster", V::Str(Arc::from(*disaster)));
    }
    if let Some(place) = municipality(text) {
        if !place.is_empty() {
            values.insert("municipality", V::Str(Arc::from(place.as_str())));
        }
    }
    let conditions: Vec<V> = CONDITIONS
        .iter()
        .filter(|(_, terms)| terms.iter().any(|term| folded.contains(&*casefold(term))))
        .map(|(label, _)| V::Str(Arc::from(*label)))
        .collect();
    if !conditions.is_empty() {
        values.insert("facility_conditions", V::List(conditions));
    }
    Ok(values)
}

// ---- The offline router's fallback -----------------------------------------------------------------

/// `str(value or "")` of a scalar of `_to_llm_contract`.
fn contract_text(value: Option<&V>) -> String {
    match value {
        None | Some(V::Null) => String::new(),
        Some(V::Str(text)) => text.to_string(),
        Some(V::Int(0)) => String::new(),
        Some(V::Int(number)) => number.to_string(),
        Some(V::Big(text)) => text.to_string(),
        Some(V::Float(number)) if *number == 0.0 => String::new(),
        Some(V::Float(number)) => python_float_repr(*number),
        Some(_) => String::new(),
    }
}

/// Whether the fallback (`_to_llm_contract` of the values) passes the prompt's schema: each scalar
/// within its `maxLength` (in code points). The structured conditions are dumps of their models,
/// which pass.
fn fallback_fits(values: &Values) -> bool {
    const LIMITS: [(&str, usize); 9] = [
        ("municipality", 160),
        ("disaster", 80),
        ("evacuees", 12),
        ("duration_days", 16),
        ("gross_area_m2", 16),
        ("accessible_toilet_count", 4),
        ("staff_count", 4),
        ("seed", 12),
        ("season", 40),
    ];
    LIMITS.iter().all(|(field, limit)| contract_text(values.get(field)).chars().count() <= *limit)
}

/// `bool(value)` of a value the rules set.
fn truthy(value: Option<&V>) -> bool {
    value.is_some_and(V::is_truthy)
}

/// `parse_scenario_text(text, router=offline router)`: what the server's `scenario-preview` reads
/// of a text without an API key. A text that is blank or longer than 4,000 code points, a number
/// Python's `float` refuses and a request `InitialScenarioRequest` refuses are `Invalid`, as the
/// ValueError and ValidationError Python raises; what the request contract does not compute is
/// `Unsupported`.
pub fn parse(text: &str) -> Result<TextParse, Refusal> {
    let chars: Vec<char> = text.chars().collect();
    if chars.iter().all(|char| re_space(*char)) || chars.len() > MAX_INPUT_LENGTH {
        return Err(Refusal::Invalid("scenario text must contain 1..4000 characters".into()));
    }
    let folded = casefold(text);
    let text_chars = Text { chars };
    let explicit = explicit_fields(&text_chars, &folded)?;
    let rule_fields: Vec<&'static str> = explicit.keys().copied().collect();
    let fits = fallback_fits(&explicit);
    let (source, mut warnings) = if fits { ("rules_fallback", vec![FALLBACK_WARNING]) } else { ("rules", vec![FALLBACK_WARNING]) };
    // `_merge_explicit` of the fallback read back: the conditions the rules left unset, at their
    // defaults (the request's defaults too, so only their origin is written here).
    let extracted: &[&'static str] =
        if fits { &["operational_conditions", "population_conditions", "evacuation_selection"] } else { &[] };
    let missing: Vec<&'static str> = REQUIRED.into_iter().filter(|field| !truthy(explicit.get(field))).collect();
    let mut request = None;
    if missing.is_empty() {
        let mut origins: Vec<(&str, &str)> = Vec::new();
        let mut users = rule_fields.clone();
        users.sort_unstable();
        origins.extend(users.iter().map(|field| (*field, "user")));
        let mut added: Vec<&str> = extracted.iter().copied().filter(|field| !rule_fields.contains(field)).collect();
        added.sort_unstable();
        origins.extend(added.iter().map(|field| (*field, "llm_extracted")));
        for field in ["duration_days", "seed", "accessible_toilet_count"] {
            if !origins.iter().any(|(name, _)| *name == field) {
                origins.push((field, "default"));
            }
        }
        let value = |field: &str| explicit.get(field).cloned().filter(|value| *value != V::Null);
        let mut fields: Vec<(&str, V)> = vec![
            ("municipality", value("municipality").unwrap_or(V::Null)),
            ("municipality_code", value("municipality_code").unwrap_or(V::Null)),
            ("disaster", value("disaster").unwrap_or(V::Null)),
            ("evacuees", value("evacuees").unwrap_or(V::Null)),
            ("duration_days", value("duration_days").unwrap_or(V::Int(7))),
            ("seed", value("seed").unwrap_or(V::Int(2026))),
            ("gross_area_m2", value("gross_area_m2").unwrap_or(V::Null)),
            ("accessible_toilet_count", value("accessible_toilet_count").unwrap_or(V::Int(1))),
            ("staff_count", value("staff_count").unwrap_or(V::Null)),
            ("season", value("season").unwrap_or(V::Null)),
            ("facility_conditions", value("facility_conditions").unwrap_or(V::List(Vec::new()))),
        ];
        if let Some(conditions) = value("operational_conditions") {
            fields.push(("operational_conditions", conditions));
        }
        fields.push((
            "field_origins",
            V::Map(origins.into_iter().map(|(field, origin)| (Arc::from(field), V::Str(Arc::from(origin)))).collect()),
        ));
        let document = V::Map(fields.into_iter().map(|(key, value)| (Arc::from(key), value)).collect());
        request = Some(InitialScenarioRequest::of_value(&document)?);
    }
    warnings.push(DRAFT_WARNING);
    Ok(TextParse {
        request,
        missing_fields: missing,
        source,
        warnings,
        input_sha256: crate::checkpoint::sha256_hex(text.as_bytes()),
    })
}

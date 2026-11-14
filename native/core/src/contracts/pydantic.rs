//! The part of pydantic 2.11.9 (pydantic-core 2.33) the input contracts use: a model's fields with
//! their types, constraints and defaults, `extra="forbid"`, the model and field validators, and
//! `model_dump(mode="json")` of what validates, or the `ValidationError`'s text (Issue #346).
//!
//! Each rule here is what pydantic does with the values a `V` can be, as the native host gives
//! them to `model_validate` (`contract_input` of `native/src/host.rs`): plain dicts and lists, or,
//! for an event's values, mapping proxies and tuples (`In::frozen`), whose `repr` the error texts
//! show. The fixture `native/core/tests/data/contracts_*.json` records Python's answers.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::json::python_float_repr;
use crate::text::py_repr;
use crate::value::V;

/// A model's validated fields, in the order of its class: what `model_dump(mode="json")` writes.
pub type Fields = IndexMap<Arc<str>, V>;

/// The depth at which pydantic-core's recursion guard refuses a recursive model
/// (`GuardExpression`): the root is at depth 0.
const RECURSION_LIMIT: usize = 255;

/// The length past which pydantic writes an input's `repr` as its first 25 and last 24 bytes.
const LONG_REPR: usize = 50;

/// The longest text pydantic parses as an int (Python's `sys.int_info.str_digits_check_threshold`).
const LONGEST_INT_TEXT: usize = 4300;

/// The type of a field, as pydantic's core schema has it.
pub enum S {
    Int(IntS),
    Float(FloatS),
    Str(StrS),
    /// `bool`, or `StrictBool` / `Field(strict=True)`.
    Bool { strict: bool },
    /// `Literal[...]`, its values in the order of the annotation.
    Lit(&'static [Lit]),
    /// `X | None`.
    Opt(&'static S),
    /// `list[X]` with `Field(min_length=..., max_length=...)`.
    List(&'static S, Len),
    /// `tuple[X, ...]` with its lengths.
    Tuple(&'static S, Len),
    /// `dict[K, X]`.
    Dict(&'static S, &'static S),
    Model(&'static Model),
    /// `StrictStr | StrictInt | StrictFloat | StrictBool` with `GuardExpression`'s field validator
    /// `finite_guard_value` (events/effects.py).
    GuardValue,
}

/// An `int`'s schema: `strict=True`, and its bounds.
pub struct IntS {
    pub strict: bool,
    pub ge: Option<i64>,
    pub gt: Option<i64>,
    pub le: Option<i64>,
}

/// A `float`'s schema: `strict=True`, `allow_inf_nan=False` (`finite`), and its bounds.
pub struct FloatS {
    pub strict: bool,
    pub finite: bool,
    pub ge: Option<f64>,
    pub gt: Option<f64>,
    pub le: Option<f64>,
}

/// A `str`'s schema: `strip_whitespace`, the lengths in characters and the pattern.
pub struct StrS {
    pub strip: bool,
    pub min: Option<usize>,
    pub max: Option<usize>,
    pub pattern: Option<Pattern>,
}

/// The patterns of the contracts' IDs, which pydantic matches with Rust's `regex` (`$` is the
/// end of the text only).
#[derive(Clone, Copy)]
pub enum Pattern {
    /// `Identifier` of contracts/extensions.py.
    Identifier,
    /// `StableId` of events/effects.py and `_StableId` of domain/environment.py and infrastructure.py.
    Stable,
}

impl Pattern {
    fn text(self) -> &'static str {
        match self {
            Pattern::Identifier => "^[A-Za-z0-9][A-Za-z0-9._:-]{0,63}$",
            Pattern::Stable => "^[A-Za-z0-9][A-Za-z0-9._:-]*$",
        }
    }

    fn matches(self, text: &str) -> bool {
        let mut chars = text.chars();
        let Some(first) = chars.next() else {
            return false;
        };
        let rest_fits = |char: char| char.is_ascii_alphanumeric() || matches!(char, '.' | '_' | ':' | '-');
        first.is_ascii_alphanumeric()
            && match self {
                Pattern::Identifier => {
                    let mut count = 0;
                    chars.all(|char| {
                        count += 1;
                        rest_fits(char)
                    }) && count <= 63
                }
                Pattern::Stable => chars.all(rest_fits),
            }
    }
}

/// A value of a `Literal`.
pub enum Lit {
    Int(i64),
    Str(&'static str),
}

impl Lit {
    fn value(&self) -> V {
        match self {
            Lit::Int(number) => V::Int(*number),
            Lit::Str(text) => V::Str(Arc::from(*text)),
        }
    }

    fn repr(&self) -> String {
        match self {
            Lit::Int(number) => number.to_string(),
            Lit::Str(text) => py_repr(text),
        }
    }

    /// Whether pydantic's literal lookup finds this value for `input`: a str by its text, an int
    /// as Python's dict finds it (`1 == 1.0 == True`).
    fn takes(&self, input: &V) -> bool {
        let number = match self {
            Lit::Str(text) => return matches!(input, V::Str(given) if **given == **text),
            Lit::Int(number) => *number,
        };
        match input {
            V::Int(given) => *given == number,
            V::Bool(given) => i64::from(*given) == number,
            V::Float(given) => *given == number as f64,
            _ => false,
        }
    }
}

/// `min_length` and `max_length` of a list or a tuple.
#[derive(Clone, Copy)]
pub struct Len {
    pub min: Option<usize>,
    pub max: Option<usize>,
}

/// A `mode="before"` model validator: the input it makes of a dict, or None to keep it.
pub type Before = fn(&Fields) -> Option<V>;

/// A `mode="after"` model validator: the `ValueError`'s text, or the fields as it leaves them.
pub type After = fn(&mut Fields) -> Result<(), String>;

/// A model (`extra="forbid"`): its title, its fields, its `mode="before"` and `mode="after"`
/// model validators, and whether it refers to itself (pydantic's recursion guard).
pub struct Model {
    pub name: &'static str,
    pub fields: &'static [Field],
    pub before: Option<Before>,
    pub after: Option<After>,
    pub recursive: bool,
}

/// A field: its name, its type and its default (not validated, as pydantic leaves defaults).
pub struct Field {
    pub name: &'static str,
    pub s: S,
    pub default: Option<fn() -> V>,
}

/// An input as `model_validate` is given it: a value, and whether it (`frozen`) and the values in
/// it (`kids`) are a mapping proxy or a tuple rather than a dict or a list.
#[derive(Clone, Copy)]
pub struct In<'a> {
    pub v: &'a V,
    pub frozen: bool,
    pub kids: bool,
}

impl<'a> In<'a> {
    pub fn plain(v: &'a V) -> Self {
        In { v, frozen: false, kids: false }
    }

    pub fn frozen(v: &'a V) -> Self {
        In { v, frozen: true, kids: true }
    }

    fn child<'b>(&self, v: &'b V) -> In<'b> {
        In { v, frozen: self.kids, kids: self.kids }
    }

    /// `type(input).__name__`.
    fn type_name(&self) -> &'static str {
        match self.v {
            V::Null => "NoneType",
            V::Bool(_) => "bool",
            V::Int(_) | V::Big(_) => "int",
            V::Float(_) => "float",
            V::Str(_) => "str",
            V::List(_) if self.frozen => "tuple",
            V::List(_) => "list",
            V::Map(_) if self.frozen => "mappingproxy",
            V::Map(_) => "dict",
        }
    }

    /// `repr(input)`.
    fn repr(&self) -> String {
        let mut out = String::new();
        repr_into(self.v, self.frozen, self.kids, &mut out);
        out
    }
}

/// `float.__repr__`, with `inf` and `nan`.
fn float_repr(number: f64) -> String {
    if number.is_nan() {
        "nan".to_owned()
    } else if number.is_infinite() {
        if number > 0.0 { "inf" } else { "-inf" }.to_owned()
    } else {
        python_float_repr(number)
    }
}

fn repr_into(value: &V, frozen: bool, kids: bool, out: &mut String) {
    match value {
        V::Null => out.push_str("None"),
        V::Bool(flag) => out.push_str(if *flag { "True" } else { "False" }),
        V::Int(number) => out.push_str(&number.to_string()),
        V::Big(digits) => out.push_str(digits),
        V::Float(number) => out.push_str(&float_repr(*number)),
        V::Str(text) => out.push_str(&py_repr(text)),
        V::List(items) => {
            out.push(if frozen { '(' } else { '[' });
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                repr_into(item, kids, kids, out);
            }
            if frozen && items.len() == 1 {
                out.push(',');
            }
            out.push(if frozen { ')' } else { ']' });
        }
        V::Map(map) => {
            if frozen {
                out.push_str("mappingproxy(");
            }
            out.push('{');
            for (index, (key, item)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                out.push_str(&py_repr(key));
                out.push_str(": ");
                repr_into(item, kids, kids, out);
            }
            out.push('}');
            if frozen {
                out.push(')');
            }
        }
    }
}

/// The `repr` as the error's line shows it: past 50 bytes, the first 25 and the last 24 bytes
/// (each cut back to whole characters) around `...`.
fn shortened(repr: String) -> String {
    if repr.len() <= LONG_REPR {
        return repr;
    }
    let mut head = 25;
    while !repr.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = repr.len() - 24;
    while !repr.is_char_boundary(tail) {
        tail += 1;
    }
    format!("{}...{}", &repr[..head], &repr[tail..])
}

/// A step of an error's location.
#[derive(Clone)]
pub enum Step {
    Name(Arc<str>),
    Index(usize),
}

/// One error of a `ValidationError`.
pub struct LineError {
    /// The location, innermost step first.
    loc: Vec<Step>,
    kind: &'static str,
    message: String,
    input: String,
    input_type: &'static str,
}

impl LineError {
    fn new(kind: &'static str, message: impl Into<String>, input: In<'_>) -> Self {
        LineError { loc: Vec::new(), kind, message: message.into(), input: shortened(input.repr()), input_type: input.type_name() }
    }
}

type Errors = Vec<LineError>;
type Out = Result<V, Errors>;

fn one(kind: &'static str, message: impl Into<String>, input: In<'_>) -> Out {
    Err(vec![LineError::new(kind, message, input)])
}

fn located(mut errors: Errors, steps: &[Step]) -> Errors {
    for error in &mut errors {
        error.loc.extend(steps.iter().rev().cloned());
    }
    errors
}

fn name(text: &str) -> Step {
    Step::Name(Arc::from(text))
}

/// `str(ValidationError)`: the count, then each error's location, message, type, input and the
/// line of its documentation.
pub fn text(title: &str, errors: &[LineError]) -> String {
    use std::fmt::Write;
    let count = errors.len();
    let mut out = format!("{count} validation error{} for {title}", if count == 1 { "" } else { "s" });
    for error in errors {
        out.push('\n');
        if !error.loc.is_empty() {
            for (index, step) in error.loc.iter().rev().enumerate() {
                if index > 0 {
                    out.push('.');
                }
                match step {
                    Step::Name(text) => out.push_str(text),
                    Step::Index(index) => out.push_str(&index.to_string()),
                }
            }
            out.push('\n');
        }
        let _ = write!(
            out,
            "  {} [type={}, input_value={}, input_type={}]\n    For further information visit https://errors.pydantic.dev/2.11/v/{}",
            error.message, error.kind, error.input, error.input_type, error.kind
        );
    }
    out
}

/// What a validation carries along: the depth of the recursive model.
#[derive(Default)]
pub struct Ctx {
    depth: usize,
}

/// `Model.model_validate(input)` as its fields' dump, or its errors.
pub fn model(model: &'static Model, input: In<'_>, ctx: &mut Ctx) -> Result<Fields, Errors> {
    if model.recursive {
        if ctx.depth >= RECURSION_LIMIT {
            return Err(vec![LineError::new("recursion_loop", "Recursion error - cyclic reference detected", input)]);
        }
        ctx.depth += 1;
        let result = fields(model, input, ctx);
        ctx.depth -= 1;
        result
    } else {
        fields(model, input, ctx)
    }
}

fn fields(model: &'static Model, given: In<'_>, ctx: &mut Ctx) -> Result<Fields, Errors> {
    // `mode="before"` runs on a dict (`isinstance(raw, dict)`), not on a mapping proxy. The
    // `mode="after"` validator wraps it, so that its error shows the input as given.
    let made;
    let input = given;
    let input = match (model.before, input.v) {
        (Some(before), V::Map(map)) if !input.frozen => match before(map) {
            Some(value) => {
                made = value;
                In { v: &made, ..input }
            }
            None => input,
        },
        _ => input,
    };
    let V::Map(map) = input.v else {
        return Err(vec![LineError::new("model_type", format!("Input should be a valid dictionary or instance of {}", model.name), input)]);
    };
    let mut out = Fields::with_capacity(model.fields.len());
    let mut errors = Errors::new();
    for field in model.fields {
        match map.get(field.name) {
            Some(value) => match validate(&field.s, input.child(value), ctx) {
                Ok(value) => {
                    out.insert(Arc::from(field.name), value);
                }
                Err(found) => errors.extend(located(found, &[name(field.name)])),
            },
            None => match field.default {
                Some(default) => {
                    out.insert(Arc::from(field.name), default());
                }
                None => errors.extend(located(vec![LineError::new("missing", "Field required", input)], &[name(field.name)])),
            },
        }
    }
    for (key, value) in map {
        if !model.fields.iter().any(|field| field.name == &**key) {
            errors.extend(located(vec![LineError::new("extra_forbidden", "Extra inputs are not permitted", input.child(value))], &[Step::Name(key.clone())]));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    if let Some(after) = model.after {
        if let Err(text) = after(&mut out) {
            return Err(vec![LineError::new("value_error", format!("Value error, {text}"), given)]);
        }
    }
    Ok(out)
}

fn validate(s: &'static S, input: In<'_>, ctx: &mut Ctx) -> Out {
    match s {
        S::Int(schema) => int(schema, input),
        S::Float(schema) => float(schema, input),
        S::Str(schema) => string(schema, input),
        S::Bool { strict } => boolean(*strict, input),
        S::Lit(values) => literal(values, input),
        S::Opt(inner) => match input.v {
            V::Null => Ok(V::Null),
            _ => validate(inner, input, ctx),
        },
        S::List(item, len) => list(item, *len, input, ctx),
        S::Tuple(item, len) => tuple(item, *len, input, ctx),
        S::Dict(key, value) => dict(key, value, input, ctx),
        S::Model(inner) => model(inner, input, ctx).map(V::Map),
        S::GuardValue => guard_value(input),
    }
}

// ---- Numbers -----------------------------------------------------------------------------------

/// An int pydantic made: within i64, or Python's digits.
enum Int {
    Small(i64),
    Big(Arc<str>),
}

impl Int {
    fn value(self) -> V {
        match self {
            Int::Small(number) => V::Int(number),
            Int::Big(digits) => V::Big(digits.as_ref().into()),
        }
    }

    /// Its order against an i64 bound.
    fn cmp(&self, bound: i64) -> std::cmp::Ordering {
        match self {
            Int::Small(number) => number.cmp(&bound),
            Int::Big(digits) if digits.starts_with('-') => std::cmp::Ordering::Less,
            Int::Big(_) => std::cmp::Ordering::Greater,
        }
    }

    /// Of a sign and decimal digits without leading zeros' meaning.
    fn of_digits(negative: bool, digits: &str) -> Int {
        let digits = digits.trim_start_matches('0');
        if digits.is_empty() {
            return Int::Small(0);
        }
        let signed = if negative { format!("-{digits}") } else { digits.to_owned() };
        match signed.parse::<i64>() {
            Ok(number) => Int::Small(number),
            Err(_) => Int::Big(Arc::from(signed)),
        }
    }
}

/// pydantic-core's `strip_underscores`: None where an underscore leads, ends or doubles.
fn strip_underscores(text: &str) -> Option<String> {
    if text.starts_with('_') || text.ends_with('_') || text.contains("__") {
        None
    } else {
        Some(text.replace('_', ""))
    }
}

/// pydantic-core's `strip_leading_zeros` of the digits after the sign: the zeros (and underscores
/// among them) before the first other digit go; a point keeps the zero before it; zeros alone
/// keep their last character.
fn strip_leading_zeros(body: &str) -> &str {
    let mut chars = body.char_indices();
    if !matches!(chars.next(), Some((_, '0'))) {
        return body;
    }
    loop {
        match chars.next() {
            Some((_, '0' | '_')) => {}
            Some((index, '.')) => return &body[index - 1..],
            Some((index, _)) => return &body[index..],
            None => return &body[body.len() - 1..],
        }
    }
}

/// pydantic-core's `str_as_int`: the text trimmed; its sign; its leading zeros and the zeros after
/// a point dropped; then decimal digits with single underscores between them.
fn int_of_text(text: &str, input: In<'_>) -> Result<Int, Errors> {
    let parsing = || vec![LineError::new("int_parsing", "Input should be a valid integer, unable to parse string as an integer", input)];
    let text = text.trim();
    let (negative, body) = match text.as_bytes().first() {
        Some(b'-') => (true, &text[1..]),
        Some(b'+') => (false, &text[1..]),
        _ => (false, text),
    };
    let mut body = strip_leading_zeros(body);
    if let Some(point) = body.find('.') {
        let fraction = &body[point + 1..];
        if !fraction.is_empty() && fraction.bytes().all(|byte| byte == b'0') {
            body = &body[..point];
        }
    }
    // The length counts a minus sign, not a plus.
    if body.len() + usize::from(negative) > LONGEST_INT_TEXT {
        return Err(vec![LineError::new("int_parsing_size", "Unable to parse input string as an integer, exceeded maximum size", input)]);
    }
    let digits = strip_underscores(body).ok_or_else(parsing)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(parsing());
    }
    Ok(Int::of_digits(negative, &digits))
}

/// pydantic-core's `float_as_int`.
fn int_of_float(number: f64, input: In<'_>) -> Result<Int, Errors> {
    if !number.is_finite() {
        Err(vec![LineError::new("finite_number", "Input should be a finite number", input)])
    } else if number % 1.0 != 0.0 {
        Err(vec![LineError::new("int_from_float", "Input should be a valid integer, got a number with a fractional part", input)])
    } else if (i64::MIN as f64) < number && number < (i64::MAX as f64) {
        Ok(Int::Small(number as i64))
    } else {
        Err(vec![LineError::new("int_parsing_size", "Unable to parse input string as an integer, exceeded maximum size", input)])
    }
}

fn int(schema: &IntS, input: In<'_>) -> Out {
    let int_type = || vec![LineError::new("int_type", "Input should be a valid integer", input)];
    let number = match input.v {
        V::Int(number) => Int::Small(*number),
        V::Big(digits) => Int::Big(Arc::from(&**digits)),
        V::Bool(flag) if !schema.strict => Int::Small(i64::from(*flag)),
        V::Float(number) if !schema.strict => int_of_float(*number, input)?,
        V::Str(text) if !schema.strict => int_of_text(text, input)?,
        _ => return Err(int_type()),
    };
    use std::cmp::Ordering::{Greater, Less};
    if let Some(bound) = schema.gt {
        if number.cmp(bound) != Greater {
            return one("greater_than", format!("Input should be greater than {bound}"), input);
        }
    }
    if let Some(bound) = schema.ge {
        if number.cmp(bound) == Less {
            return one("greater_than_equal", format!("Input should be greater than or equal to {bound}"), input);
        }
    }
    if let Some(bound) = schema.le {
        if number.cmp(bound) == Greater {
            return one("less_than_equal", format!("Input should be less than or equal to {bound}"), input);
        }
    }
    Ok(number.value())
}

/// `float(int)`, or None where Python raises OverflowError.
fn float_of_digits(digits: &str) -> Option<f64> {
    digits.parse::<f64>().ok().filter(|number| number.is_finite())
}

fn float(schema: &FloatS, input: In<'_>) -> Out {
    let float_type = || vec![LineError::new("float_type", "Input should be a valid number", input)];
    let number = match input.v {
        V::Float(number) => *number,
        V::Int(number) => *number as f64,
        V::Big(digits) => float_of_digits(digits).ok_or_else(float_type)?,
        V::Bool(flag) if !schema.strict => f64::from(u8::from(*flag)),
        V::Str(text) if !schema.strict => {
            let parsed = text.trim().parse::<f64>().ok().or_else(|| strip_underscores(text).and_then(|stripped| stripped.parse::<f64>().ok()));
            parsed.ok_or_else(|| vec![LineError::new("float_parsing", "Input should be a valid number, unable to parse string as a number", input)])?
        }
        _ => return Err(float_type()),
    };
    if schema.finite && !number.is_finite() {
        return one("finite_number", "Input should be a finite number", input);
    }
    if let Some(bound) = schema.gt {
        if !(number > bound) {
            return one("greater_than", format!("Input should be greater than {bound}"), input);
        }
    }
    if let Some(bound) = schema.ge {
        if !(number >= bound) {
            return one("greater_than_equal", format!("Input should be greater than or equal to {bound}"), input);
        }
    }
    if let Some(bound) = schema.le {
        if !(number <= bound) {
            return one("less_than_equal", format!("Input should be less than or equal to {bound}"), input);
        }
    }
    Ok(V::Float(number))
}

// ---- Texts, bools and literals -----------------------------------------------------------------

fn plural(count: usize) -> &'static str {
    if count == 1 {
        ""
    } else {
        "s"
    }
}

fn string(schema: &StrS, input: In<'_>) -> Out {
    let V::Str(given) = input.v else {
        return one("string_type", "Input should be a valid string", input);
    };
    let text: &str = if schema.strip { given.trim() } else { given };
    let length = text.chars().count();
    if let Some(min) = schema.min {
        if length < min {
            return one("string_too_short", format!("String should have at least {min} character{}", plural(min)), input);
        }
    }
    if let Some(max) = schema.max {
        if length > max {
            return one("string_too_long", format!("String should have at most {max} character{}", plural(max)), input);
        }
    }
    if let Some(pattern) = schema.pattern {
        if !pattern.matches(text) {
            return one("string_pattern_mismatch", format!("String should match pattern '{}'", pattern.text()), input);
        }
    }
    Ok(if text.len() == given.len() { input.v.clone() } else { V::Str(Arc::from(text)) })
}

fn boolean(strict: bool, input: In<'_>) -> Out {
    let parsing = || one("bool_parsing", "Input should be a valid boolean, unable to interpret input", input);
    match input.v {
        V::Bool(flag) => Ok(V::Bool(*flag)),
        _ if strict => one("bool_type", "Input should be a valid boolean", input),
        V::Int(0) => Ok(V::Bool(false)),
        V::Int(1) => Ok(V::Bool(true)),
        V::Int(_) => parsing(),
        V::Float(number) if *number == 0.0 => Ok(V::Bool(false)),
        V::Float(number) if *number == 1.0 => Ok(V::Bool(true)),
        V::Str(text) => match text.to_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Ok(V::Bool(false)),
            "1" | "on" | "t" | "true" | "y" | "yes" => Ok(V::Bool(true)),
            _ => parsing(),
        },
        _ => one("bool_type", "Input should be a valid boolean", input),
    }
}

fn literal(values: &'static [Lit], input: In<'_>) -> Out {
    // The lookup of an int literal takes the input as a strict int first: a Python int outside
    // i64 is refused as too large.
    if matches!(input.v, V::Big(_)) && values.iter().any(|value| matches!(value, Lit::Int(_))) {
        return one("int_parsing_size", "Unable to parse input string as an integer, exceeded maximum size", input);
    }
    if let Some(found) = values.iter().find(|value| value.takes(input.v)) {
        return Ok(found.value());
    }
    let reprs: Vec<String> = values.iter().map(Lit::repr).collect();
    let expected = match reprs.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => reprs.concat(),
    };
    one("literal_error", format!("Input should be {expected}"), input)
}

/// `StrictStr | StrictInt | StrictFloat | StrictBool` (smart mode: each input matches one member
/// exactly), then `finite_guard_value`.
fn guard_value(input: In<'_>) -> Out {
    match input.v {
        V::Str(text) if text.chars().count() > 256 => one("value_error", "Value error, guard string values are limited to 256 characters", input),
        V::Float(number) if !number.is_finite() => one("value_error", "Value error, guard numeric values must be finite", input),
        V::Str(_) | V::Int(_) | V::Big(_) | V::Float(_) | V::Bool(_) => Ok(input.v.clone()),
        _ => Err(vec![
            located(vec![LineError::new("string_type", "Input should be a valid string", input)], &[name("str")]),
            located(vec![LineError::new("int_type", "Input should be a valid integer", input)], &[name("int")]),
            located(vec![LineError::new("float_type", "Input should be a valid number", input)], &[name("float")]),
            located(vec![LineError::new("bool_type", "Input should be a valid boolean", input)], &[name("bool")]),
        ]
        .into_iter()
        .flatten()
        .collect()),
    }
}

// ---- Containers --------------------------------------------------------------------------------

fn too_long(kind: &str, max: usize, actual: usize, input: In<'_>) -> Errors {
    vec![LineError::new("too_long", format!("{kind} should have at most {max} item{} after validation, not {actual}", plural(max)), input)]
}

fn too_short(kind: &str, min: usize, actual: usize, input: In<'_>) -> LineError {
    LineError::new("too_short", format!("{kind} should have at least {min} item{} after validation, not {actual}", plural(min)), input)
}

/// `list[X]`: each item counts toward `max_length`, which refuses at once; `min_length` is asked
/// only when every item validated.
fn list(item: &'static S, len: Len, input: In<'_>, ctx: &mut Ctx) -> Out {
    let V::List(items) = input.v else {
        return one("list_type", "Input should be a valid list", input);
    };
    let mut out = Vec::with_capacity(items.len());
    let mut errors = Errors::new();
    for (index, value) in items.iter().enumerate() {
        let result = validate(item, input.child(value), ctx);
        if len.max.is_some_and(|max| index + 1 > max) {
            return Err(too_long("List", len.max.unwrap_or_default(), items.len(), input));
        }
        match result {
            Ok(value) => out.push(value),
            Err(found) => errors.extend(located(found, &[Step::Index(index)])),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    if let Some(min) = len.min {
        if out.len() < min {
            return Err(vec![too_short("List", min, out.len(), input)]);
        }
    }
    Ok(V::List(out))
}

/// `tuple[X, ...]`: only the items that validate count toward `max_length`, which refuses at
/// once; `min_length` is asked of them after the items' errors.
fn tuple(item: &'static S, len: Len, input: In<'_>, ctx: &mut Ctx) -> Out {
    let V::List(items) = input.v else {
        return one("tuple_type", "Input should be a valid tuple", input);
    };
    let mut out = Vec::with_capacity(items.len());
    let mut errors = Errors::new();
    for (index, value) in items.iter().enumerate() {
        match validate(item, input.child(value), ctx) {
            Ok(value) => {
                if len.max.is_some_and(|max| out.len() + 1 > max) {
                    return Err(too_long("Tuple", len.max.unwrap_or_default(), items.len(), input));
                }
                out.push(value);
            }
            Err(found) => errors.extend(located(found, &[Step::Index(index)])),
        }
    }
    if let Some(min) = len.min {
        if out.len() < min {
            errors.push(too_short("Tuple", min, out.len(), input));
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(V::List(out))
}

/// `dict[K, X]`: each key, then its value.
fn dict(key: &'static S, value: &'static S, input: In<'_>, ctx: &mut Ctx) -> Out {
    let V::Map(map) = input.v else {
        return one("dict_type", "Input should be a valid dictionary", input);
    };
    let mut out = IndexMap::with_capacity(map.len());
    let mut errors = Errors::new();
    for (name, item) in map {
        let given = V::Str(name.clone());
        let key_out = validate(key, In::plain(&given), ctx);
        let value_out = validate(value, input.child(item), ctx);
        let key_out = match key_out {
            Ok(V::Str(text)) => Some(text),
            Ok(_) => None,
            Err(found) => {
                errors.extend(located(found, &[Step::Name(name.clone()), self::name("[key]")]));
                None
            }
        };
        match value_out {
            Ok(value) => {
                if let Some(text) = key_out {
                    out.insert(text, value);
                }
            }
            Err(found) => errors.extend(located(found, &[Step::Name(name.clone())])),
        }
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(V::Map(out))
}

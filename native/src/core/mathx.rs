//! Float operations of the model. This file is the only caller of exp, expm1, pow and fma;
//! `native/clippy.toml` bans the `f64` methods elsewhere.
//!
//! exp, expm1, pow and fma come from the `libm` crate, pure Rust pinned to one version, so they
//! give the same bits on Linux, Windows and macOS (docs/rust-port/step4-plan.md, section 9).
//! Before the model's version 2 they called the platform's libm, as CPython does. hypot, fsum,
//! sum and round are CPython 3.12's own algorithms; of libm they use only `fma`.
//!
//! Where CPython raised, exp, expm1 and pow return a `MathError`, which becomes Python's
//! exception with Python's text.

use std::cmp::Ordering;

use pyo3::exceptions::{PyOverflowError, PyValueError, PyZeroDivisionError};
use pyo3::prelude::*;
use pyo3::types::{PyFloat, PyInt};

/// Where `math.exp`, `math.expm1` or `float ** float` raised in CPython 3.12 on Linux.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum MathError {
    /// `math_1` of mathmodule.c: an infinity from a finite argument.
    Range,
    /// `float_pow`: the result overflowed, which CPython reports from errno.
    PowerRange,
    /// `float_pow`: `0.0 ** negative`.
    ZeroToNegative,
    /// `float_pow`: a negative base and a fractional exponent. CPython returns a complex here,
    /// which no rule can use; this is the ValueError Python 2 raised.
    NegativeToFraction,
}

impl MathError {
    /// `str()` of the exception CPython raised.
    pub(crate) fn text(self) -> &'static str {
        match self {
            MathError::Range => "math range error",
            MathError::PowerRange => "(34, 'Numerical result out of range')",
            MathError::ZeroToNegative => "0.0 cannot be raised to a negative power",
            MathError::NegativeToFraction => "negative number cannot be raised to a fractional power",
        }
    }
}

impl From<MathError> for PyErr {
    fn from(error: MathError) -> Self {
        let text = error.text();
        match error {
            MathError::Range | MathError::PowerRange => PyOverflowError::new_err(text),
            MathError::ZeroToNegative => PyZeroDivisionError::new_err(text),
            MathError::NegativeToFraction => PyValueError::new_err(text),
        }
    }
}

/// `math_1` of mathmodule.c for exp and expm1, which give no NaN from a non-NaN: an infinity
/// from a finite argument raises. An underflow to zero or a subnormal does not.
fn math_1(x: f64, result: f64) -> Result<f64, MathError> {
    if result.is_infinite() && x.is_finite() {
        return Err(MathError::Range);
    }
    Ok(result)
}

/// `math.exp`.
#[inline]
pub(crate) fn exp(x: f64) -> Result<f64, MathError> {
    math_1(x, libm::exp(x))
}

/// `math.expm1`.
#[inline]
pub(crate) fn expm1(x: f64) -> Result<f64, MathError> {
    math_1(x, libm::expm1(x))
}

/// `x * y + z` rounded once. `f64::mul_add` calls the platform's `fma` where the CPU has none.
#[inline]
pub(crate) fn fma(x: f64, y: f64, z: f64) -> f64 {
    libm::fma(x, y, z)
}

fn is_odd_integer(value: f64) -> bool {
    (value.abs() % 2.0) == 1.0
}

/// `base ** exponent` of two floats (an int operand is converted first), with the special cases
/// of `float_pow`. A subnormal result is kept: CPython raised OverflowError for one where glibc
/// set ERANGE, which depends on the platform.
pub(crate) fn pow(base: f64, exponent: f64) -> Result<f64, MathError> {
    if exponent == 0.0 {
        return Ok(1.0);
    }
    if base.is_nan() {
        return Ok(base);
    }
    if exponent.is_nan() {
        return Ok(if base == 1.0 { 1.0 } else { exponent });
    }
    if exponent.is_infinite() {
        let size = base.abs();
        return Ok(if size == 1.0 {
            1.0
        } else if (exponent > 0.0) == (size > 1.0) {
            exponent.abs()
        } else {
            0.0
        });
    }
    if base.is_infinite() {
        let odd = is_odd_integer(exponent);
        return Ok(if exponent > 0.0 {
            if odd {
                base
            } else {
                base.abs()
            }
        } else if odd {
            0.0_f64.copysign(base)
        } else {
            0.0
        });
    }
    if base == 0.0 {
        if exponent < 0.0 {
            return Err(MathError::ZeroToNegative);
        }
        return Ok(if is_odd_integer(exponent) { base } else { 0.0 });
    }
    let mut size = base;
    let mut negate = false;
    if base < 0.0 {
        if exponent != exponent.floor() {
            return Err(MathError::NegativeToFraction);
        }
        size = -base;
        negate = is_odd_integer(exponent);
    }
    if size == 1.0 {
        return Ok(if negate { -1.0 } else { 1.0 });
    }
    let result = libm::pow(size, exponent);
    if result.is_infinite() {
        return Err(MathError::PowerRange);
    }
    Ok(if negate { -result } else { result })
}

/// `max(first, second)` of floats: the first on a tie or when a NaN makes the comparison false.
#[inline]
pub(crate) fn py_max(first: f64, second: f64) -> f64 {
    if second > first {
        second
    } else {
        first
    }
}

/// `math.isclose(first, second)` with its default tolerances: a relative one of 1e-09 and no
/// absolute one.
pub(crate) fn py_is_close(first: f64, second: f64) -> bool {
    if first == second {
        return true;
    }
    if first.is_infinite() || second.is_infinite() {
        return false;
    }
    let difference = (second - first).abs();
    difference <= (1e-09 * second).abs() || difference <= (1e-09 * first).abs()
}

/// `min(first, second)` of floats, with the same ties as `py_max`.
#[inline]
pub(crate) fn py_min(first: f64, second: f64) -> f64 {
    if second < first {
        second
    } else {
        first
    }
}

/// A Python number of either type, for the places where the type survives into the output:
/// `max(0, x)` returns the int `0`, which JSON writes as `0` and not `0.0`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    /// An exact int that fits i64 or an exact float; anything else is left to Python.
    pub(crate) fn from_py(value: &Bound<'_, PyAny>) -> Option<Self> {
        if let Ok(number) = value.downcast_exact::<PyFloat>() {
            return Some(Num::Float(number.value()));
        }
        value
            .downcast_exact::<PyInt>()
            .ok()?
            .extract()
            .ok()
            .map(Num::Int)
    }

    pub(crate) fn to_py<'py>(self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        use pyo3::IntoPyObjectExt;
        match self {
            Num::Int(value) => value.into_bound_py_any(py),
            Num::Float(value) => value.into_bound_py_any(py),
        }
    }

    /// The float Python converts it to where it meets a float: exact for an int within 2**53.
    pub(crate) fn value(self) -> f64 {
        match self {
            Num::Int(value) => value as f64,
            Num::Float(value) => value,
        }
    }

    /// `-x`: an int stays an int, so `-0` is the int 0 and not the float -0.0.
    pub(crate) fn neg(self) -> Num {
        match self {
            Num::Int(value) => Num::Int(-value),
            Num::Float(value) => Num::Float(-value),
        }
    }
}

/// An int against a float by exact value, as `float_richcompare` does; `None` with a NaN.
fn compare_int_float(int: i64, float: f64) -> Option<Ordering> {
    if float.is_nan() {
        return None;
    }
    const LIMIT: f64 = 9_223_372_036_854_775_808.0;
    if float >= LIMIT {
        return Some(Ordering::Less);
    }
    if float < -LIMIT {
        return Some(Ordering::Greater);
    }
    let whole = float.trunc();
    Some(int.cmp(&(whole as i64)).then_with(|| {
        let rest = float - whole;
        if rest > 0.0 {
            Ordering::Less
        } else if rest < 0.0 {
            Ordering::Greater
        } else {
            Ordering::Equal
        }
    }))
}

fn compare(first: Num, second: Num) -> Option<Ordering> {
    match (first, second) {
        (Num::Int(a), Num::Int(b)) => Some(a.cmp(&b)),
        (Num::Float(a), Num::Float(b)) => a.partial_cmp(&b),
        (Num::Int(a), Num::Float(b)) => compare_int_float(a, b),
        (Num::Float(a), Num::Int(b)) => compare_int_float(b, a).map(Ordering::reverse),
    }
}

/// `max(first, second)` with its operand's type: `max_num(Num::Int(0), Num::Float(x))` is
/// `max(0, x)`, the int `0` unless `x > 0`.
pub(crate) fn max_num(first: Num, second: Num) -> Num {
    if compare(second, first) == Some(Ordering::Greater) {
        second
    } else {
        first
    }
}

/// `min(first, second)` with its operand's type.
pub(crate) fn min_num(first: Num, second: Num) -> Num {
    if compare(second, first) == Some(Ordering::Less) {
        second
    } else {
        first
    }
}

/// `sum(values)` of at least one float as CPython 3.12 computes it: the int start 0 plus the
/// first item, then Neumaier's compensated summation, adding a finite non-zero compensation at
/// the end. `sum([])` is the int 0; use `sum_nums` where the list can be empty.
pub(crate) fn py_sum(values: &[f64]) -> f64 {
    let Some((first, rest)) = values.split_first() else {
        return 0.0;
    };
    let mut total = 0.0 + first;
    let mut compensation = 0.0;
    for &value in rest {
        let next = total + value;
        if total.abs() >= value.abs() {
            compensation += (total - next) + value;
        } else {
            compensation += (value - next) + total;
        }
        total = next;
    }
    if compensation != 0.0 && compensation.is_finite() {
        total += compensation;
    }
    total
}

/// `sum(values)` of ints and floats, as CPython 3.12 computes it where C long has 64 bits (Linux
/// and macOS; on Windows it has 32, and the model takes 64 on every OS). Ints add exactly until
/// the first float, whose addition converts the total; later floats are compensated and later
/// ints added without compensation. `None` when ints leave i64, which Python carries on with as
/// big ints.
pub(crate) fn sum_nums(values: &[Num]) -> Option<Num> {
    let mut exact: i64 = 0;
    let mut items = values.iter();
    let mut total = loop {
        match items.next() {
            None => return Some(Num::Int(exact)),
            Some(Num::Int(value)) => {
                exact = exact.checked_add(*value)?;
            }
            Some(Num::Float(value)) => break exact as f64 + value,
        }
    };
    let mut compensation = 0.0;
    for item in items {
        match *item {
            Num::Float(value) => {
                let next = total + value;
                if total.abs() >= value.abs() {
                    compensation += (total - next) + value;
                } else {
                    compensation += (value - next) + total;
                }
                total = next;
            }
            Num::Int(value) => total += value as f64,
        }
    }
    if compensation != 0.0 && compensation.is_finite() {
        total += compensation;
    }
    Some(Num::Float(total))
}

/// math.fsum as CPython 3.12 computes it (Shewchuk's partials with its half-even correction
/// of the last bit), for finite values whose partial sums stay finite.
pub(crate) fn py_fsum(values: &[f64]) -> Option<f64> {
    let mut partials: Vec<f64> = Vec::with_capacity(8);
    for &value in values {
        if !value.is_finite() {
            return None;
        }
        let mut x = value;
        let mut kept = 0;
        for index in 0..partials.len() {
            let mut y = partials[index];
            if x.abs() < y.abs() {
                std::mem::swap(&mut x, &mut y);
            }
            let hi = x + y;
            let yr = hi - x;
            let lo = y - yr;
            if lo != 0.0 {
                partials[kept] = lo;
                kept += 1;
            }
            x = hi;
        }
        partials.truncate(kept);
        if x != 0.0 {
            if !x.is_finite() {
                return None;
            }
            partials.push(x);
        }
    }
    let mut n = partials.len();
    let mut hi = 0.0;
    if n > 0 {
        n -= 1;
        hi = partials[n];
        let mut lo = 0.0;
        while n > 0 {
            let x = hi;
            n -= 1;
            let y = partials[n];
            hi = x + y;
            let yr = hi - x;
            lo = y - yr;
            if lo != 0.0 {
                break;
            }
        }
        if n > 0 && ((lo < 0.0 && partials[n - 1] < 0.0) || (lo > 0.0 && partials[n - 1] > 0.0)) {
            let y = lo * 2.0;
            let x = hi + y;
            let yr = x - hi;
            if y == yr {
                hi = x;
            }
        }
    }
    Some(hi)
}

/// `round(x, ndigits)` of a float for `ndigits >= 0`: the nearest decimal of the exact binary
/// value, half to even, read back as the nearest float. CPython does this with David Gay's
/// dtoa and strtod; Rust's exact formatting and parsing give the same digits and float.
/// `None` for negative `ndigits`, which no rule uses.
pub(crate) fn py_round(x: f64, ndigits: i32) -> Option<f64> {
    if ndigits < 0 {
        return None;
    }
    // NDIGITS_MAX in floatobject.c: more digits than any double has.
    if !x.is_finite() || ndigits > 323 {
        return Some(x);
    }
    format!("{:.*}", ndigits as usize, x).parse().ok()
}

/// `round(x)` of a float, half to even, which Python returns as an int; `None` for what is not
/// finite (Python raises) or does not fit i64.
pub(crate) fn py_round_int(x: f64) -> Option<i64> {
    let rounded = x.round_ties_even();
    if !rounded.is_finite() || !(-9.223372036854775e18..=9.223372036854775e18).contains(&rounded) {
        return None;
    }
    Some(rounded as i64)
}

/// `frexp(value).1` of a positive finite value.
fn binary_exponent(value: f64) -> i32 {
    if value.is_subnormal() {
        // 2**54 moves it into the normal range without rounding.
        return binary_exponent(value * 18_014_398_509_481_984.0) - 54;
    }
    ((value.to_bits() >> 52) & 0x7ff) as i32 - 1022
}

/// `ldexp(1.0, exponent)` for -1074 <= exponent <= 1023.
fn power_of_two(exponent: i32) -> f64 {
    if exponent >= -1022 {
        f64::from_bits(((exponent + 1023) as u64) << 52)
    } else {
        f64::from_bits(1u64 << (exponent + 1074))
    }
}

/// `vector_norm` of mathmodule.c on absolute values with their largest given.
fn vector_norm(values: &mut [f64], largest: f64, found_nan: bool) -> f64 {
    if largest.is_infinite() {
        return largest;
    }
    if found_nan {
        return f64::NAN;
    }
    if largest == 0.0 || values.len() <= 1 {
        return largest;
    }
    let exponent = binary_exponent(largest);
    if exponent < -1023 {
        for value in values.iter_mut() {
            *value /= f64::MIN_POSITIVE;
        }
        return f64::MIN_POSITIVE * vector_norm(values, largest / f64::MIN_POSITIVE, found_nan);
    }
    let scale = power_of_two(-exponent);
    let (mut sum, mut frac1, mut frac2) = (1.0_f64, 0.0_f64, 0.0_f64);
    let mut add = |square_hi: f64, square_lo: f64| {
        let next = sum + square_hi;
        let lost = square_hi - (next - sum);
        sum = next;
        frac1 += square_lo;
        frac2 += lost;
        sum - 1.0 + (frac1 + frac2)
    };
    let mut approximate = 0.0;
    for &value in values.iter() {
        let scaled = value * scale;
        let square = scaled * scaled;
        approximate = add(square, fma(scaled, scaled, -square));
    }
    let mut root = approximate.sqrt();
    let square = -root * root;
    let correction = add(square, fma(-root, root, -square));
    root += correction / (2.0 * root);
    root / scale
}

/// `math.hypot(*values)`: CPython's own algorithm, not libm's.
pub(crate) fn hypot(values: &[f64]) -> f64 {
    let mut sizes: Vec<f64> = Vec::with_capacity(values.len());
    let (mut largest, mut found_nan) = (0.0_f64, false);
    for &value in values {
        let size = value.abs();
        found_nan |= size.is_nan();
        if size > largest {
            largest = size;
        }
        sizes.push(size);
    }
    vector_norm(&mut sizes, largest, found_nan)
}

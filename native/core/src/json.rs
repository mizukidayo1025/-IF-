//! The bytes of canonical JSON that need no Python: strings as `json.dumps(ensure_ascii=False)`
//! writes them, and floats as `float.__repr__` and the encoder write them.

/// A JSON string as json.dumps(ensure_ascii=False) writes it.
pub fn write_json_string(out: &mut Vec<u8>, text: &str) {
    // json's ensure_ascii=False escapes only '"', '\\' and control characters.
    out.push(b'"');
    let bytes = text.as_bytes();
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let escape: &[u8] = match byte {
            b'"' => b"\\\"",
            b'\\' => b"\\\\",
            b'\n' => b"\\n",
            b'\r' => b"\\r",
            b'\t' => b"\\t",
            0x08 => b"\\b",
            0x0c => b"\\f",
            0x00..=0x1f => b"",
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..index]);
        if escape.is_empty() {
            out
                .extend_from_slice(format!("\\u{:04x}", byte).as_bytes());
        } else {
            out.extend_from_slice(escape);
        }
        start = index + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

/// float.__repr__: the shortest digits that read back as the same float, in fixed
/// notation when the decimal point falls within 16 digits of the first digit.
pub fn python_float_repr(value: f64) -> String {
    if value == 0.0 {
        return if value.is_sign_negative() { "-0.0" } else { "0.0" }.to_owned();
    }
    let mut buffer = ryu::Buffer::new();
    let shortest = buffer.format_finite(value);
    let (negative, unsigned) = match shortest.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, shortest),
    };
    let (mantissa, exponent) = match unsigned.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().unwrap()),
        None => (unsigned, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let mut digits: String = format!("{whole}{fraction}");
    let mut point = whole.len() as i32 + exponent;
    let leading = digits.len() - digits.trim_start_matches('0').len();
    digits.drain(..leading);
    point -= leading as i32;
    let trimmed = digits.trim_end_matches('0').len();
    digits.truncate(trimmed);

    let mut text = String::with_capacity(32);
    if negative {
        text.push('-');
    }
    let count = digits.len() as i32;
    if point <= -4 || point > 16 {
        text.push_str(&digits[..1]);
        if count > 1 {
            text.push('.');
            text.push_str(&digits[1..]);
        }
        let shown = point - 1;
        text.push('e');
        text.push(if shown < 0 { '-' } else { '+' });
        text.push_str(&format!("{:02}", shown.abs()));
    } else if point <= 0 {
        text.push_str("0.");
        text.push_str(&"0".repeat((-point) as usize));
        text.push_str(&digits);
    } else if point >= count {
        text.push_str(&digits);
        text.push_str(&"0".repeat((point - count) as usize));
        text.push_str(".0");
    } else {
        text.push_str(&digits[..point as usize]);
        text.push('.');
        text.push_str(&digits[point as usize..]);
    }
    text
}

/// A NaN or an infinity, which canonical JSON leaves to Python's `json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotFinite;

/// The JSON of a finite float as the canonical encoder writes it: ryu's digits in its range,
/// `float.__repr__` outside it.
pub fn write_float(out: &mut Vec<u8>, value: f64) -> Result<(), NotFinite> {
    if !value.is_finite() {
        return Err(NotFinite);
    }
    // In this range ryu and float.__repr__ both write the shortest digits in fixed
    // notation with at least one digit after the point.
    if (1e-4..1e16).contains(&value.abs()) {
        let mut buffer = ryu::Buffer::new();
        out.extend_from_slice(buffer.format_finite(value).as_bytes());
    } else {
        out.extend_from_slice(python_float_repr(value).as_bytes());
    }
    Ok(())
}

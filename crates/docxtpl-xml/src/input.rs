//! Shared input-scanning utilities: position conversion, entity recognition,
//! character validity.

use crate::error::XmlError;
use crate::names;

/// Built-in general entity name → character.
pub(crate) fn builtin_entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => None,
    }
}

/// Decode `&...;` in strict mode (`pos` points at `&`); returns the
/// character and the byte offset just after the entity.
pub(crate) fn decode_entity_strict(input: &str, pos: usize) -> Result<(char, usize), XmlError> {
    let bytes = input.as_bytes();
    if pos + 1 >= bytes.len() {
        return Err(XmlError::at(input, pos, "unterminated entity reference"));
    }
    if bytes[pos + 1] == b'#' {
        let (ch, end) = decode_numeric(input, pos + 2)?;
        if !is_valid_char(ch) {
            return Err(XmlError::at(
                input,
                pos,
                format!(
                    "character reference U+{:04X} is not a valid XML character",
                    ch as u32
                ),
            ));
        }
        Ok((ch, end))
    } else if let Some((name, end)) = read_entity_name(input, pos + 1) {
        if end < input.len() && input.as_bytes()[end] == b';' {
            match builtin_entity(name) {
                Some(ch) => Ok((ch, end + 1)),
                None => Err(XmlError::at(
                    input,
                    pos,
                    format!("undefined entity '{name}' (only built-in entities allowed)"),
                )),
            }
        } else {
            Err(XmlError::at(
                input,
                pos,
                "entity reference missing semicolon",
            ))
        }
    } else {
        Err(XmlError::at(input, pos, "invalid entity reference"))
    }
}

/// Read an entity name (XML Name); the caller verifies that a semicolon
/// follows.
fn read_entity_name(input: &str, pos: usize) -> Option<(&str, usize)> {
    names::read_name(input, pos)
}

/// Parse a numeric character reference; `digits_pos` points just after `#`
/// (at `x`/a digit). A terminating semicolon is required.
fn decode_numeric(input: &str, digits_pos: usize) -> Result<(char, usize), XmlError> {
    let bytes = input.as_bytes();
    let (radix, mut p) =
        if bytes.get(digits_pos) == Some(&b'x') || bytes.get(digits_pos) == Some(&b'X') {
            (16, digits_pos + 1)
        } else {
            (10, digits_pos)
        };
    let start = p;
    let mut value: u32 = 0;
    while let Some(&b) = bytes.get(p) {
        let d = match b {
            b'0'..=b'9' => (b - b'0') as u32,
            b'a'..=b'f' if radix == 16 => (b - b'a' + 10) as u32,
            b'A'..=b'F' if radix == 16 => (b - b'A' + 10) as u32,
            _ => break,
        };
        value = value
            .checked_mul(radix)
            .and_then(|v| v.checked_add(d))
            .unwrap_or(u32::MAX);
        p += 1;
    }
    if p == start || bytes.get(p) != Some(&b';') {
        return Err(XmlError::at(
            input,
            start - 1,
            "invalid numeric character reference",
        ));
    }
    char::from_u32(value).map(|ch| (ch, p + 1)).ok_or_else(|| {
        XmlError::at(
            input,
            start - 1,
            "numeric character reference out of Unicode range",
        )
    })
}

/// Try to parse a numeric character reference following strict syntax (used
/// by lenient mode).
///
/// `hash_pos` points at `#`. On success returns the character and the offset
/// just after `;`.
pub(crate) fn try_numeric_ref(input: &str, hash_pos: usize) -> Option<(char, usize)> {
    let bytes = input.as_bytes();
    let (radix, mut p) =
        if bytes.get(hash_pos + 1) == Some(&b'x') || bytes.get(hash_pos + 1) == Some(&b'X') {
            (16, hash_pos + 2)
        } else {
            (10, hash_pos + 1)
        };
    let start = p;
    let mut value: u64 = 0;
    while let Some(&b) = bytes.get(p) {
        let d = match b {
            b'0'..=b'9' => (b - b'0') as u64,
            b'a'..=b'f' if radix == 16 => (b - b'a' + 10) as u64,
            b'A'..=b'F' if radix == 16 => (b - b'A' + 10) as u64,
            _ => break,
        };
        // A malformed reference can contain an arbitrarily long digit run.
        // Saturation preserves the eventual "out of Unicode range" result
        // without allowing debug or fuzz builds to panic on integer overflow.
        value = value.saturating_mul(radix as u64).saturating_add(d);
        p += 1;
    }
    if p == start || bytes.get(p) != Some(&b';') {
        return None;
    }
    let ch = u32::try_from(value).ok().and_then(char::from_u32)?;
    Some((ch, p + 1))
}

/// XML 1.0 valid characters (including #x9 #xA #xD).
pub(crate) fn is_valid_char(c: char) -> bool {
    matches!(c, '\u{09}' | '\u{0A}' | '\u{0D}')
        || matches!(c as u32, 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

/// Compute the 1-based line and column numbers for a byte offset (the
/// column counts Unicode scalars).
pub(crate) fn line_col(input: &str, offset: usize) -> (usize, usize) {
    let mut bounded = offset.min(input.len());
    while !input.is_char_boundary(bounded) {
        bounded -= 1;
    }
    let prefix = &input[..bounded];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = match prefix.rfind('\n') {
        Some(nl) => input[nl + 1..bounded].chars().count() + 1,
        None => prefix.chars().count() + 1,
    };
    (line, col)
}

/// Skip XML whitespace and return the new byte offset.
pub(crate) fn skip_ws(input: &str, pos: usize) -> usize {
    let mut p = pos.min(input.len());
    while !input.is_char_boundary(p) {
        p -= 1;
    }
    let tail = &input[p..];
    for c in tail.chars() {
        if !matches!(c, ' ' | '\t' | '\n' | '\r') {
            break;
        }
        p += c.len_utf8();
    }
    p
}

#[cfg(test)]
mod tests {
    use super::{line_col, skip_ws};

    #[test]
    fn position_helpers_tolerate_non_boundary_offsets() {
        let input = "a\u{feff} b";
        assert_eq!(line_col(input, 2), (1, 2));
        assert_eq!(skip_ws(input, 2), 1);
        assert_eq!(skip_ws(input, usize::MAX), input.len());
    }
}

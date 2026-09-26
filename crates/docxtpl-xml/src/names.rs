//! XML 1.0 name-character predicates (NameStartChar / NameChar, Unicode
//! ranges).
//!
//! Covers only the ranges defined by the XML 1.0 (fifth edition) grammar;
//! the lenient parser uses them to replicate the range of bad entity names
//! consumed by libxml2 recover.

/// Return whether the character may start an XML Name (colon included; the
/// lexical layer imposes no namespace constraints).
pub(crate) fn is_name_start(c: char) -> bool {
    matches!(c, ':' | '_' | 'A'..='Z' | 'a'..='z') || in_name_start_range(c)
}

/// Return whether the character may follow the first character of an XML Name.
pub(crate) fn is_name_char(c: char) -> bool {
    if is_name_start(c) {
        return true;
    }
    matches!(c, '-' | '.' | '0'..='9' | '\u{00B7}')
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
}

/// XML whitespace characters: space, tab, line feed, carriage return.
pub(crate) fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// Read a complete XML Name (including colons) starting at `pos`; returns
/// the name slice and the ending byte offset.
pub(crate) fn read_name(s: &str, pos: usize) -> Option<(&str, usize)> {
    // Parser recovery can advance byte-by-byte over malformed input, so `pos`
    // is not guaranteed to be a UTF-8 character boundary. Treat such a
    // position as "no name" instead of panicking while slicing.
    let tail = s.get(pos..)?;
    let mut chars = tail.char_indices();
    let first = chars.next()?;
    if !is_name_start(first.1) {
        return None;
    }
    let mut end = pos + first.0 + first.1.len_utf8();
    for (i, c) in chars {
        if !is_name_char(c) {
            break;
        }
        end = pos + i + c.len_utf8();
    }
    Some((&s[pos..end], end))
}

/// Non-ASCII ranges of NameStartChar (XML 1.0 §2.3).
fn in_name_start_range(c: char) -> bool {
    matches!(c as u32,
        0x00C0..=0x00D6
        | 0x00D8..=0x00F6
        | 0x00F8..=0x02FF
        | 0x0370..=0x037D
        | 0x037F..=0x1FFF
        | 0x200C..=0x200D
        | 0x2070..=0x218F
        | 0x2C00..=0x2FEF
        | 0x3001..=0xD7FF
        | 0xF900..=0xFDCF
        | 0xFDF0..=0xFFFD
        | 0x10000..=0xEFFFF)
}

#[cfg(test)]
mod tests {
    use super::read_name;

    #[test]
    fn read_name_rejects_non_boundary_and_out_of_range_offsets() {
        let input = "\u{feff}name";
        assert_eq!(read_name(input, 1), None);
        assert_eq!(read_name(input, input.len() + 1), None);
    }
}

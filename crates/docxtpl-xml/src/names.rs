//! XML 1.0 名称字符判定（NameStartChar / NameChar，Unicode 版本）。
//!
//! 仅覆盖 XML 1.0 第五版文法规定的区间；宽松解析器用它复刻
//! libxml2 recover 对坏实体名的吞食范围。

/// 判断字符是否可作为 XML Name 的首字符（含冒号，词法层不做 NS 约束）。
pub(crate) fn is_name_start(c: char) -> bool {
    matches!(c, ':' | '_' | 'A'..='Z' | 'a'..='z') || in_name_start_range(c)
}

/// 判断字符是否可作为 XML Name 的后续字符。
pub(crate) fn is_name_char(c: char) -> bool {
    if is_name_start(c) {
        return true;
    }
    matches!(c, '-' | '.' | '0'..='9' | '\u{00B7}')
        || ('\u{0300}'..='\u{036F}').contains(&c)
        || ('\u{203F}'..='\u{2040}').contains(&c)
}

/// XML 空白字符：空格、制表、换行、回车。
pub(crate) fn is_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r')
}

/// 从 `pos` 处读取一个完整 XML Name（含冒号），返回名称切片与结束字节偏移。
pub(crate) fn read_name(s: &str, pos: usize) -> Option<(&str, usize)> {
    let mut chars = s[pos..].char_indices();
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

/// NameStartChar 的非 ASCII 区间（XML 1.0 §2.3）。
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

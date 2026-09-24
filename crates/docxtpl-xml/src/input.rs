//! 输入扫描共用工具：位置换算、实体识别、字符合法性。

use crate::error::XmlError;
use crate::names;

/// 内建通用实体名 → 字符。
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

/// 严格模式解码 `&...;`（`pos` 指向 `&`），返回字符与实体结束后的字节偏移。
pub(crate) fn decode_entity_strict(input: &str, pos: usize) -> Result<(char, usize), XmlError> {
    let bytes = input.as_bytes();
    if pos + 1 >= bytes.len() {
        return Err(XmlError::at(input, pos, "未结束的实体引用"));
    }
    if bytes[pos + 1] == b'#' {
        let (ch, end) = decode_numeric(input, pos + 2)?;
        if !is_valid_char(ch) {
            return Err(XmlError::at(
                input,
                pos,
                format!("字符引用 U+{:04X} 不是合法 XML 字符", ch as u32),
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
                    format!("未定义的实体 '{name}'（仅允许内建实体）"),
                )),
            }
        } else {
            Err(XmlError::at(input, pos, "实体引用缺少分号"))
        }
    } else {
        Err(XmlError::at(input, pos, "非法实体引用"))
    }
}

/// 读取实体名（XML Name），要求其后紧跟分号由调用方校验。
fn read_entity_name(input: &str, pos: usize) -> Option<(&str, usize)> {
    names::read_name(input, pos)
}

/// 解析数字字符引用，`digits_pos` 指向 `#` 之后（`x`/数字），要求分号结尾。
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
        return Err(XmlError::at(input, start - 1, "非法数字字符引用"));
    }
    char::from_u32(value)
        .map(|ch| (ch, p + 1))
        .ok_or_else(|| XmlError::at(input, start - 1, "数字字符引用超出 Unicode 范围"))
}

/// 尝试按严格语法解析数字字符引用（宽松模式使用）。
///
/// `hash_pos` 指向 `#`。成功时返回字符与 `;` 之后的偏移。
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
        value = value * (radix as u64) + d;
        p += 1;
    }
    if p == start || bytes.get(p) != Some(&b';') {
        return None;
    }
    let ch = u32::try_from(value).ok().and_then(char::from_u32)?;
    Some((ch, p + 1))
}

/// XML 1.0 合法字符（含 #x9 #xA #xD）。
pub(crate) fn is_valid_char(c: char) -> bool {
    matches!(c, '\u{09}' | '\u{0A}' | '\u{0D}')
        || matches!(c as u32, 0x20..=0xD7FF | 0xE000..=0xFFFD | 0x10000..=0x10FFFF)
}

/// 计算字节偏移对应的 1 起始行号与列号（列按 Unicode 标量计数）。
pub(crate) fn line_col(input: &str, offset: usize) -> (usize, usize) {
    let bounded = offset.min(input.len());
    let prefix = &input[..bounded];
    let line = prefix.bytes().filter(|&b| b == b'\n').count() + 1;
    let col = match prefix.rfind('\n') {
        Some(nl) => input[nl + 1..bounded].chars().count() + 1,
        None => prefix.chars().count() + 1,
    };
    (line, col)
}

/// 跳过 XML 空白，返回新的字节偏移。
pub(crate) fn skip_ws(input: &str, pos: usize) -> usize {
    let mut p = pos;
    for c in input[pos..].chars() {
        if !matches!(c, ' ' | '\t' | '\n' | '\r') {
            break;
        }
        p += c.len_utf8();
    }
    p
}

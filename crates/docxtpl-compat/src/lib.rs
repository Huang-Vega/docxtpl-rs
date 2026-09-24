//! docxtpl-compat：上游兼容层。
//!
//! 收纳有上游证据（docxtpl 0.20.2 `docxtpl/template.py`）的特殊语义规则：
//! [`patch_xml`] 把 `DocxTemplate.patch_xml` 的正则变换逐条移植到 Rust
//! （fancy-regex，支持前瞻/后顾），包括分隔符拆分愈合、标签内跨 run 合并、
//! colspan/cellbg 注入、空白控制、tr/tc/p/r 结构化标签提升、vm/hm 单元格合并
//! 与 clean_tags 实体检出。不得加入任何无上游证据的补丁。
//!
//! 所有变换均为纯字符串处理，顺序固定、不可配置，语义见 ADR-002。

use fancy_regex::{Captures, Regex};
use std::sync::OnceLock;

mod listing;

pub use listing::resolve_listing;

/// 对齐 docxtpl 0.20.2 `DocxTemplate.patch_xml`。纯字符串变换，顺序固定，不可配置。
///
/// 13 个步骤严格按上游 `docxtpl/template.py` L84–303 的顺序执行，全部等价于
/// `flags=re.DOTALL` 下的 `re.sub`（替换全部非重叠匹配）。
#[must_use]
pub fn patch_xml(src: &str) -> String {
    let src = step_delimiter_heal(src);
    let src = step_strip_tags_inside_markers(&src);
    let src = step_colspan(&src);
    let src = step_cellbg(&src);
    let src = step_ensure_space_preservation(&src);
    let src = step_split_r_tag(&src);
    let src = step_trim_left(&src);
    let src = step_trim_right(&src);
    let src = step_structured_tags(&src);
    let src = step_comment_structured_tags(&src);
    let src = step_v_merge(&src);
    let src = step_h_merge(&src);
    step_clean_tags(&src)
}

// ===========================================================================
// 通用替换工具
// ===========================================================================

/// 等价 Python `re.sub(pattern, repl, src, flags=re.DOTALL)`：
/// 按 `captures_iter` 从左到右拼接未匹配切片与回调结果。
///
/// fancy-regex 在病态回溯超预算时会返回 `Err`；此时保留剩余后缀原文、停止替换。
/// docxtpl corpus 为有界 XML 文档，不会触发该路径（与上游无语义出入）。
fn sub(re: &Regex, src: &str, f: impl Fn(&Captures) -> String) -> String {
    let mut out = String::with_capacity(src.len());
    let mut last = 0usize;
    for item in re.captures_iter(src) {
        let caps = match item {
            Ok(caps) => caps,
            Err(_) => break,
        };
        // group 0 在正常匹配中必然存在；缺失时无法定位切片边界，结束替换。
        let m = match caps.get(0) {
            Some(m) => m,
            None => break,
        };
        out.push_str(&src[last..m.start()]);
        out.push_str(&f(&caps));
        last = m.end();
    }
    out.push_str(&src[last..]);
    out
}

/// 等价 `re.sub(pattern, repl, src, count=1)`：只替换第一次匹配；无匹配时原样返回。
fn replace_first(re: &Regex, src: &str, replacement: &str) -> String {
    if let Ok(Some(caps)) = re.captures(src) {
        if let Some(m) = caps.get(0) {
            let mut out = String::with_capacity(src.len() + replacement.len());
            out.push_str(&src[..m.start()]);
            out.push_str(replacement);
            out.push_str(&src[m.end()..]);
            return out;
        }
    }
    src.to_owned()
}

/// 取捕获组字符串；未参与匹配的分组按空串处理（上游对应分组均保证参与）。
fn cap<'t>(caps: &'t Captures, idx: usize) -> &'t str {
    caps.get(idx).map_or("", |m| m.as_str())
}

/// 拼接若干字符串切片。
fn concat(parts: &[&str]) -> String {
    let total = parts.iter().map(|s| s.len()).sum();
    let mut out = String::with_capacity(total);
    for part in parts {
        out.push_str(part);
    }
    out
}

/// 定义一个只编译一次的正则访问函数（编译失败属程序 bug，仅此处允许 expect）。
macro_rules! define_regex {
    ($(#[doc = $doc:literal] $name:ident = $pat:literal;)*) => {
        $(
            #[doc = $doc]
            fn $name() -> &'static Regex {
                static RE: OnceLock<Regex> = OnceLock::new();
                RE.get_or_init(|| Regex::new($pat).expect(concat!("invalid regex: ", $pat)))
            }
        )*
    };
}

define_regex! {
    /// 步骤 1：`(?<={)(<[^>]*>)+(?=[{#%])|(?<=[%}#])(<[^>]*>)+(?=\})`（DOTALL）。
    re_delimiter_heal = r"(?s)(?<=\{)(<[^>]*>)+(?=[{#%])|(?<=[%}#])(<[^>]*>)+(?=\})";

    /// 步骤 2 外层：`{%` / `{#` / `{{` 起、到对应闭合分隔符之前（DOTALL）。
    re_tag_outer = r"(?s)\{%(?:(?!%\}).)*|{#(?:(?!#\}).)*|\{\{(?:(?!\}\}).)*";

    /// 步骤 2 内层：标签内部的 run 边界 `</w:t>…<w:t>`（DOTALL）。
    re_run_boundary = r"(?s)</w:t>.*?(<w:t>|<w:t [^>]*>)";

    /// 步骤 3 外层：承载 `{% colspan expr %}` 的单元格（DOTALL）。
    re_colspan_outer = r"(?s)(<w:tc[ >](?:(?!<w:tc[ >]).)*)\{%\s*colspan\s+([^%]*)\s*%\}(.*?</w:tc>)";

    /// 步骤 3/4：删除空文本 run（DOTALL）。
    re_empty_run = r"(?s)<w:r[ >](?:(?!<w:r[ >]).)*<w:t></w:t>.*?</w:r>";

    /// 步骤 3：首个 `<w:gridSpan .../>`（DOTALL）。
    re_gridspan_any = r"(?s)<w:gridSpan[^/]*/>";

    /// 步骤 4 外层：承载 `{% cellbg expr %}` 的单元格（DOTALL）。
    re_cellbg_outer = r"(?s)(<w:tc[ >](?:(?!<w:tc[ >]).)*)\{%\s*cellbg\s+([^%]*)\s*%\}(.*?</w:tc>)";

    /// 步骤 4：首个 `<w:shd .../>`（DOTALL）。
    re_shd_any = r"(?s)<w:shd[^/]*/>";

    /// 步骤 3/4：`<w:tcPr ...>` 开标签（DOTALL）。
    re_tcpr_open = r"(?s)(<w:tcPr[^>]*>)";

    /// 步骤 5：裸 `<w:t>` 内含 `{{...}}` / `{%...%}`（DOTALL）。
    re_xml_space = r"(?s)<w:t>((?:(?!<w:t>).)*)(\{\{.*?\}\}|\{%.*?%\})";

    /// 步骤 6：`{{r ...}}` / `{%r ...%}`（DOTALL）。
    re_r_split = r"(?s)(\{\{r\s.*?\}\}|\{%r\s.*?%\})";

    /// 步骤 7：`{%-` 与前文合并（DOTALL）。
    re_trim_left = r"(?s)</w:t>(?:(?!</w:t>).)*?\{%-";

    /// 步骤 8：`-%}` 与后文合并（DOTALL）。
    re_trim_right = r"(?s)-%\}(?:(?!<w:t[ >]|\{%|\{\{).)*?<w:t[^>]*?>";

    /// 步骤 11 外层：含 `{% vm %}` 的单元格（DOTALL）。
    re_vm_outer = r"(?s)<w:tc[ >](?:(?!<w:tc[ >]).)*?\{%\s*vm\s*%\}.*?</w:tc[ >]";

    /// 步骤 11 内层：tcPr 尾 → `<w:t>`、vm 前后文本、`</w:t>`（DOTALL）。
    re_vm_inner = r"(?s)(</w:tcPr[ >].*?<w:t(?:.*?)>)(.*?)(?:\{%\s*vm\s*%\})(.*?)(</w:t>)";

    /// 步骤 12 外层：含 `{% hm %}` 的单元格（DOTALL）。
    re_hm_outer = r"(?s)<w:tc[ >](?:(?!<w:tc[ >]).)*?\{%\s*hm\s*%\}.*?</w:tc[ >]";

    /// 步骤 12 已含 gridSpan：数值替换为乘法表达式（DOTALL）。
    re_hm_gridspan_num = r#"(?s)(w:gridSpan w:val=")(\d+)(")"#;

    /// 步骤 12：删除 `{% hm %}` 标签本身（DOTALL）。
    re_hm_tag = r"(?s)\{%\s*hm\s*%\}";

    /// 步骤 12 未含 gridSpan：新增 gridSpan 的内层结构（DOTALL）。
    re_hm_inner = r"(?s)(</w:tcPr[ >].*?<w:t(?:.*?)>)(.*?)(?:\{%\s*hm\s*%\})(.*?)(</w:t>)";

    /// 步骤 13：标签内部（`{{`/`{%` 之后，`}}`/`%}` 之前，DOTALL）。
    re_clean_tags = r"(?s)(?<=\{[{%])(.*?)(?=[}%]})";
}

/// 步骤 9 模板，`{y}` 代入 tr/tc/p/r。
const STRUCT_PATTERN: &str =
    r"(?s)<w:{y}[ >](?:(?!<w:{y}[ >]).)*(\{%|\{\{){y} ([^}%]*(?:%\}|\}\})).*?</w:{y}>";

/// 步骤 10 模板，`{y}` 代入 tr/tc/p。
const COMMENT_STRUCT_PATTERN: &str =
    r"(?s)<w:{y}[ >](?:(?!<w:{y}[ >]).)*(\{#){y} ([^}#]*(?:#\})).*?</w:{y}>";

const STRUCT_TAGS: [&str; 4] = ["tr", "tc", "p", "r"];
const COMMENT_STRUCT_TAGS: [&str; 3] = ["tr", "tc", "p"];

static STRUCT_REGEXES: [OnceLock<Regex>; 4] = [const { OnceLock::new() }; 4];
static COMMENT_STRUCT_REGEXES: [OnceLock<Regex>; 3] = [const { OnceLock::new() }; 3];

/// 步骤 9：按 tr/tc/p/r 顺序各取一个只编译一次的正则。
fn struct_regex(index: usize) -> &'static Regex {
    STRUCT_REGEXES[index].get_or_init(|| {
        let pattern = STRUCT_PATTERN.replace("{y}", STRUCT_TAGS[index]);
        Regex::new(&pattern).expect("invalid structured-tag regex")
    })
}

/// 步骤 10：按 tr/tc/p 顺序各取一个只编译一次的正则。
fn comment_struct_regex(index: usize) -> &'static Regex {
    COMMENT_STRUCT_REGEXES[index].get_or_init(|| {
        let pattern = COMMENT_STRUCT_PATTERN.replace("{y}", COMMENT_STRUCT_TAGS[index]);
        Regex::new(&pattern).expect("invalid comment-structured-tag regex")
    })
}

// ===========================================================================
// 13 个步骤，逐一对应上游 template.py
// ===========================================================================

/// 步骤 1（上游 L89–95）：分隔符愈合。
///
/// 删除 `{` 与 `{`/`%`/`#` 之间、以及 `%`/`}`/`#` 与 `}` 之间夹着的 XML 标记，
/// 使被 Word 拆进不同 run 的 `{{`/`}}`/`{%`/`%}`/`{#`/`#}` 重新愈合。
fn step_delimiter_heal(src: &str) -> String {
    sub(re_delimiter_heal(), src, |_| String::new())
}

/// 步骤 2（上游 L97–110）：标签内跨 run 合并。
///
/// 对每个从 `{%`/`{#`/`{{` 开始的标签匹配，删除其内部全部
/// `</w:t>…<w:t>` 边界（含带属性的 `<w:t ...>`）。
fn step_strip_tags_inside_markers(src: &str) -> String {
    sub(re_tag_outer(), src, |caps| {
        let whole = cap(caps, 0);
        sub(re_run_boundary(), whole, |_| String::new())
    })
}

/// 步骤 3（上游 L112–133）：`{% colspan expr %}`。
///
/// 承载单元格 = g1 + g3（标签本身丢弃）：先删空文本 run，再删首个
/// `<w:gridSpan .../>`，然后在每个 `<w:tcPr ...>` 后注入
/// `<w:gridSpan w:val="{{expr}}"/>`（expr 为 g2 原文）。
fn step_colspan(src: &str) -> String {
    sub(re_colspan_outer(), src, |caps| {
        let cell = concat(&[cap(caps, 1), cap(caps, 3)]);
        let cell = sub(re_empty_run(), &cell, |_| String::new());
        let cell = replace_first(re_gridspan_any(), &cell, "");
        let injection = concat(&["<w:gridSpan w:val=\"{{", cap(caps, 2), "}}\"/>"]);
        sub(re_tcpr_open(), &cell, |c| concat(&[cap(c, 1), &injection]))
    })
}

/// 步骤 4（上游 L135–156）：`{% cellbg expr %}`。
///
/// 同步骤 3：删空文本 run、删首个 `<w:shd .../>`，在 `<w:tcPr ...>` 后注入
/// `<w:shd w:val="clear" w:color="auto" w:fill="{{expr}}"/>`。
fn step_cellbg(src: &str) -> String {
    sub(re_cellbg_outer(), src, |caps| {
        let cell = concat(&[cap(caps, 1), cap(caps, 3)]);
        let cell = sub(re_empty_run(), &cell, |_| String::new());
        let cell = replace_first(re_shd_any(), &cell, "");
        let injection = concat(&[
            "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"{{",
            cap(caps, 2),
            "}}\"/>",
        ]);
        sub(re_tcpr_open(), &cell, |c| concat(&[cap(c, 1), &injection]))
    })
}

/// 步骤 5（上游 L158–164）：含标签的裸 `<w:t>` 补 `xml:space="preserve"`。
///
/// 仅匹配无任何属性的 `<w:t>`；替换为
/// `<w:t xml:space="preserve">` + g1 + g2，标签之后的原文保留。
fn step_ensure_space_preservation(src: &str) -> String {
    sub(re_xml_space(), src, |caps| {
        concat(&["<w:t xml:space=\"preserve\">", cap(caps, 1), cap(caps, 2)])
    })
}

/// 步骤 6（上游 L165–170）：`{{r …}}` / `{%r …%}` 独立成 run。
///
/// 标签前闭合当前 run，再以无格式新 run 承载标签，最后再开一个无格式 run。
fn step_split_r_tag(src: &str) -> String {
    sub(re_r_split(), src, |caps| {
        let tag = cap(caps, 1);
        concat(&[
            "</w:t></w:r><w:r><w:t xml:space=\"preserve\">",
            tag,
            "</w:t></w:r><w:r><w:t xml:space=\"preserve\">",
        ])
    })
}

/// 步骤 7（上游 L172–173）：`{%-` 与上一段文本合并。
///
/// `</w:t>` 之后到 `{%-` 之间（不能跨越另一个 `</w:t>`）整体替换为 `{%`。
fn step_trim_left(src: &str) -> String {
    sub(re_trim_left(), src, |_| "{%".to_owned())
}

/// 步骤 8（上游 L174–177）：`-%}` 与下一段文本合并。
///
/// `-%}` 之后到下一个 `<w:t...>` 之间不得出现 `<w:t `/`<w:t>`/`{%`/`{{`，
/// 整段替换为 `%}`。
fn step_trim_right(src: &str) -> String {
    sub(re_trim_right(), src, |_| "%}".to_owned())
}

/// 步骤 9（上游 L179–188）：结构化标签提升，严格按 tr → tc → p → r 各执行一次。
///
/// 把整个承载元素 `<w:y …>…{%y …%}…</w:y>`（或 `{{y …}}`）替换为
/// g1（`{%` 或 `{{`）+ 一个空格 + g2（标签正文含闭合分隔符）。
fn step_structured_tags(src: &str) -> String {
    let mut src = src.to_owned();
    for index in 0..STRUCT_TAGS.len() {
        src = sub(struct_regex(index), &src, |caps| {
            concat(&[cap(caps, 1), " ", cap(caps, 2)])
        });
    }
    src
}

/// 步骤 10（上游 L190–197）：注释结构化标签提升，按 tr → tc → p（不含 r）。
///
/// 同步骤 9，但作用于 `{#y …#}`。
fn step_comment_structured_tags(src: &str) -> String {
    let mut src = src.to_owned();
    for index in 0..COMMENT_STRUCT_TAGS.len() {
        src = sub(comment_struct_regex(index), &src, |caps| {
            concat(&[cap(caps, 1), " ", cap(caps, 2)])
        });
    }
    src
}

/// 步骤 11（上游 L199–228）：`{% vm %}` 垂直合并。
///
/// 在单元格内 `</w:tcPr>` 与 `<w:t>` 之间插入
/// `<w:vMerge w:val="{% if loop.first %}restart{% else %}continue{% endif %}"/>`，
/// 并用 `{% if loop.first %}…{% endif %}` 包住 vm 标签两侧的文本（g2+g3），
/// `</w:t>`（g4）始终保留。内层无匹配时整格原样返回。
fn step_v_merge(src: &str) -> String {
    sub(re_vm_outer(), src, |caps| {
        let cell = cap(caps, 0);
        sub(re_vm_inner(), cell, |m| {
            concat(&[
                "<w:vMerge w:val=\"{% if loop.first %}restart{% else %}continue{% endif %}\"/>",
                cap(m, 1),
                "{% if loop.first %}",
                cap(m, 2),
                cap(m, 3),
                "{% endif %}",
                cap(m, 4),
            ])
        })
    })
}

/// 步骤 12（上游 L230–287）：`{% hm %}` 水平合并。
///
/// - 单元格已含 `w:gridSpan`：把数值替换为 `{{ N * loop.length }}`，删空 hm 标签；
/// - 否则：在 `</w:tcPr>` 与 `<w:t>` 之间插入
///   `<w:gridSpan w:val="{{ loop.length }}"/>`，保留 g1–g4；
///
/// 两种分支的返回值都整体外包 `{% if loop.first %}…{% endif %}`。
fn step_h_merge(src: &str) -> String {
    sub(re_hm_outer(), src, |caps| {
        let cell = cap(caps, 0);
        let patched = if cell.contains("w:gridSpan") {
            let multiplied = sub(re_hm_gridspan_num(), cell, |m| {
                concat(&[cap(m, 1), "{{ ", cap(m, 2), " * loop.length }}", cap(m, 3)])
            });
            sub(re_hm_tag(), &multiplied, |_| String::new())
        } else {
            sub(re_hm_inner(), cell, |m| {
                concat(&[
                    "<w:gridSpan w:val=\"{{ loop.length }}\"/>",
                    cap(m, 1),
                    cap(m, 2),
                    cap(m, 3),
                    cap(m, 4),
                ])
            })
        };
        concat(&["{% if loop.first %}", &patched, "{% endif %}"])
    })
}

/// 步骤 13（上游 L289–301）：clean_tags。
///
/// 仅作用于 `{{`/`{%` 之后、`}}`/`%}` 之前的标签内部，按上游固定顺序做字面替换：
/// `&#8216;`→`'`、`&lt;`→`<`、`&gt;`→`>`、
/// 左右双引号（U+201C/U+201D）→`"`、左右单引号（U+2018/U+2019）→`'`。
fn step_clean_tags(src: &str) -> String {
    sub(re_clean_tags(), src, |caps| {
        cap(caps, 0)
            .replace("&#8216;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace(['\u{201C}', '\u{201D}'], "\"")
            .replace(['\u{2018}', '\u{2019}'], "'")
    })
}

//! 内联小用例：13 个步骤每步至少 2 个（正常 + 边界）。
//!
//! 每个用例都直接对 [`docxtpl_compat::patch_xml`] 的完整管线输出做字符串断言；
//! 期望值逐条由上游 docxtpl 0.20.2 `DocxTemplate.patch_xml` 生成（oracle 对照），
//! 因此也覆盖步骤间的真实联动（例如步骤 3 删除承载标签后变空的 run 会被空 run
//! 正则清掉、步骤 6 拆出的中间 run 随后被步骤 9 的 `r` 提升）。

use docxtpl_compat::patch_xml;

fn check(input: &str, expected: &str) {
    let actual = patch_xml(input);
    assert_eq!(
        actual, expected,
        "input={input:?}\n  got: {actual:?}\n  exp: {expected:?}"
    );
}

// ---- 步骤 1：分隔符愈合 ---------------------------------------------------

#[test]
fn step1_delimiter_heal() {
    // 开侧：`{` 与 `{` 之间的标记删除。
    check(r"{</w:t><w:t>{", "{{");
    // 闭侧：`%` 与 `}` 之间的标记删除。
    check(r"%<x>}", "%}");
    // `#` 两侧（{# ... #}）同时愈合。
    check(r"{<a>#foo#<b>}", "{#foo#}");
    // 边界：stray `{%` 中间没有 XML 标记，整体保持不变。
    check(r"hello {% world", "hello {% world");
}

// ---- 步骤 2：标签内跨 run 合并 --------------------------------------------

#[test]
fn step2_strip_tags_inside_markers() {
    check(r"{%</w:t><w:t>x%}", "{%x%}");
    check(r#"{{</w:t><w:t xml:space="preserve">v}}"#, "{{v}}");
    check(r"{#</w:t><w:t>c#}", "{#c#}");
    // 边界：标签内没有 run 边界时原样保留。
    check(r"{%a%}", "{%a%}");
}

// ---- 步骤 3：colspan -------------------------------------------------------

#[test]
fn step3_colspan() {
    check(
        r#"<w:tc><w:tcPr><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t>{% colspan span %}</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:gridSpan w:val="{{span }}"/><w:tcW w:w="100"/></w:tcPr><w:p></w:p></w:tc>"#,
    );
    // 已存在 gridSpan：先删首个旧 gridSpan，承载标签的空 run 一并删除，再注入。
    check(
        r#"<w:tc><w:tcPr><w:gridSpan w:val="2"/><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t></w:t></w:r><w:r><w:t>{% colspan 3 %}</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:gridSpan w:val="{{3 }}"/><w:tcW w:w="100"/></w:tcPr><w:p></w:p></w:tc>"#,
    );
    // 边界：不在 w:tc 内，colspan 不生效。
    check(r"{% colspan x %}", "{% colspan x %}");
}

// ---- 步骤 4：cellbg --------------------------------------------------------

#[test]
fn step4_cellbg() {
    // 原有 shd 被删除后注入新 shd；承载 run 变空被清除。
    check(
        r#"<w:tc><w:tcPr><w:shd w:val="clear" w:fill="auto"/></w:tcPr><w:p><w:r><w:t>{% cellbg color %}</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="{{color }}"/></w:tcPr><w:p></w:p></w:tc>"#,
    );
    // 无 shd 时直接注入。
    check(
        r#"<w:tc><w:tcPr></w:tcPr><w:p><w:r><w:t>{% cellbg c %}</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="{{c }}"/></w:tcPr><w:p></w:p></w:tc>"#,
    );
}

// ---- 步骤 5：xml:space="preserve" -----------------------------------------

#[test]
fn step5_space_preservation() {
    check(
        r"<w:t>{{x}}</w:t>",
        r#"<w:t xml:space="preserve">{{x}}</w:t>"#,
    );
    check(
        r"<w:t>{% if a %}</w:t>",
        r#"<w:t xml:space="preserve">{% if a %}</w:t>"#,
    );
    // 边界：带属性的 w:t 不重复添加。
    check(
        r#"<w:t xml:space="preserve">{{x}}</w:t>"#,
        r#"<w:t xml:space="preserve">{{x}}</w:t>"#,
    );
    // 边界：不含标签的纯文本 w:t 不变。
    check(r"<w:t>hello</w:t>", r"<w:t>hello</w:t>");
}

// ---- 步骤 6：{{r }} / {%r %} 拆 run（随后步骤 9 会提升中间 run） -----------

#[test]
fn step6_r_tag_split() {
    check(
        r"<w:t>{{r x}}</w:t>",
        r#"<w:t xml:space="preserve"></w:t></w:r>{{ x}}<w:r><w:t xml:space="preserve"></w:t>"#,
    );
    check(
        r"<w:t>{%r if a%}</w:t>",
        r#"<w:t xml:space="preserve"></w:t></w:r>{% if a%}<w:r><w:t xml:space="preserve"></w:t>"#,
    );
    // 边界：r 后必须有空白，`{{rx}}` 只受步骤 5 影响。
    check(
        r"<w:t>{{rx}}</w:t>",
        r#"<w:t xml:space="preserve">{{rx}}</w:t>"#,
    );
}

// ---- 步骤 7：{%- 左合并 ----------------------------------------------------

#[test]
fn step7_trim_left() {
    check(r"</w:t>FOO{%-bar%}", "{%bar%}");
    // 边界：守卫不得跨越另一个 </w:t>，改由第二个 </w:t> 处起匹配。
    check(r"</w:t>A</w:t>B{%-x%}", "</w:t>A{%x%}");
    // 边界：没有 `{%-` 时不变。
    check(r"</w:t>FOO{%bar%}", "</w:t>FOO{%bar%}");
}

// ---- 步骤 8：-%} 右合并 ----------------------------------------------------

#[test]
fn step8_trim_right() {
    check(r"abc-%}XYZ<w:t>q", "abc%}q");
    check(r#"-%}x<w:t xml:space="preserve">y"#, "%}y");
    // 边界：-%} 与 <w:t> 之间出现 {{，守卫阻止跨越，整段不变。
    check(r"-%}a{{b}}<w:t>", "-%}a{{b}}<w:t>");
}

// ---- 步骤 9：tr/tc/p/r 结构化标签提升 --------------------------------------

#[test]
fn step9_structured_tags() {
    check(r"<w:p><w:r><w:t>{%p if %}</w:t></w:r></w:p>", "{% if %}");
    check(r"<w:p><w:r><w:t>{{p x}}</w:t></w:r></w:p>", "{{ x}}");
    check(
        r"<w:tr><w:tc><w:p><w:r><w:t>{%tr for%}</w:t></w:r></w:p></w:tc></w:tr>",
        "{% for%}",
    );
    // 边界：普通标签（无 y 前缀）不被提升，仅步骤 5 补 preserve。
    check(
        r"<w:p><w:r><w:t>{%x%}</w:t></w:r></w:p>",
        r#"<w:p><w:r><w:t xml:space="preserve">{%x%}</w:t></w:r></w:p>"#,
    );
}

// ---- 步骤 10：tr/tc/p 注释结构化标签提升 -----------------------------------

#[test]
fn step10_comment_structured_tags() {
    check(
        r"<w:tc><w:p><w:r><w:t>{#tc c #}</w:t></w:r></w:p></w:tc>",
        "{# c #}",
    );
    check(
        r"<w:tr><w:tc><w:r><w:t>{#tr note#}</w:t></w:r></w:tc></w:tr>",
        "{# note#}",
    );
    // 边界：y=r 不做注释提升。
    check(
        r"<w:r><w:t>{#r x #}</w:t></w:r>",
        r"<w:r><w:t>{#r x #}</w:t></w:r>",
    );
}

// ---- 步骤 11：vm 垂直合并 --------------------------------------------------

#[test]
fn step11_v_merge() {
    check(
        r#"<w:tc><w:tcPr><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t>{% vm %}A</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:tcW w:w="100"/><w:vMerge w:val="{% if loop.first %}restart{% else %}continue{% endif %}"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">{% if loop.first %}A{% endif %}</w:t></w:r></w:p></w:tc>"#,
    );
    // vm 前后都有文本：g2+g3 只在 loop.first 保留。
    check(
        r#"<w:tc><w:tcPr></w:tcPr><w:p><w:r><w:t>B{% vm %}A</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:vMerge w:val="{% if loop.first %}restart{% else %}continue{% endif %}"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">{% if loop.first %}BA{% endif %}</w:t></w:r></w:p></w:tc>"#,
    );
    // 边界：vm 不在 w:tc 内，不处理。
    check(r"{% vm %}", "{% vm %}");
}

// ---- 步骤 12：hm 水平合并 --------------------------------------------------

#[test]
fn step12_h_merge_without_gridspan() {
    check(
        r#"<w:tc><w:tcPr><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t>{% hm %}A</w:t></w:r></w:p></w:tc>"#,
        r#"{% if loop.first %}<w:tc><w:tcPr><w:tcW w:w="100"/><w:gridSpan w:val="{{ loop.length }}"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc>{% endif %}"#,
    );
    // 边界：hm 不在 w:tc 内，不处理。
    check(r"{% hm %}", "{% hm %}");
}

#[test]
fn step12_h_merge_with_gridspan() {
    // 已有 gridSpan：数值乘 loop.length，hm 标签删空，整格外包 if loop.first。
    check(
        r#"<w:tc><w:tcPr><w:gridSpan w:val="3"/><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t>{% hm %}A</w:t></w:r></w:p></w:tc>"#,
        r#"{% if loop.first %}<w:tc><w:tcPr><w:gridSpan w:val="{{ 3 * loop.length }}"/><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc>{% endif %}"#,
    );
}

// ---- 步骤 13：clean_tags ---------------------------------------------------

#[test]
fn step13_clean_tags() {
    check(r"{{a &lt; b &gt; c}}", "{{a < b > c}}");
    // 智能引号（U+201C/U+201D）还原为 ASCII 双引号。
    check("{{x|default(\u{201c}N\u{201d})}}", r#"{{x|default("N")}}"#);
    // &#8216; 左单引号实体还原。
    check(r"{{&#8216;q&#8216;}}", "{{'q'}}");
}

#[test]
fn step13_clean_tags_scope_boundaries() {
    // 边界：标签外的实体不动。
    check(r"a &lt; b", "a &lt; b");
    // 边界：{# #} 注释内部不在 clean_tags 作用域（后顾只认 {{ / {%）。
    check(r"{# a &lt; b #}", "{# a &lt; b #}");
}

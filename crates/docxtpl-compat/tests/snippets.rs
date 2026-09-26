//! Inline snippet cases: at least 2 per step (a normal case and a boundary
//! case) across the 13 steps.
//!
//! Each case makes string assertions directly on the full-pipeline output of
//! [`docxtpl_compat::patch_xml`]; expected values were generated individually
//! by upstream docxtpl 0.20.2 `DocxTemplate.patch_xml` (oracle comparison), so
//! they also cover real interactions between steps (e.g. runs emptied after
//! step 3 removes the carrying tag are swept away by the empty-run regex, and
//! the middle run split out by step 6 is later promoted by step 9's `r`).

use docxtpl_compat::patch_xml;

fn check(input: &str, expected: &str) {
    let actual = patch_xml(input);
    assert_eq!(
        actual, expected,
        "input={input:?}\n  got: {actual:?}\n  exp: {expected:?}"
    );
}

// ---- Step 1: delimiter healing --------------------------------------------

#[test]
fn step1_delimiter_heal() {
    // Opening side: markup between `{` and `{` is removed.
    check(r"{</w:t><w:t>{", "{{");
    // Closing side: markup between `%` and `}` is removed.
    check(r"%<x>}", "%}");
    // Both sides of `#` ({# ... #}) are healed at once.
    check(r"{<a>#foo#<b>}", "{#foo#}");
    // Boundary: a stray `{%` with no XML markup in between stays unchanged.
    check(r"hello {% world", "hello {% world");
}

// ---- Step 2: cross-run merging inside tags --------------------------------

#[test]
fn step2_strip_tags_inside_markers() {
    check(r"{%</w:t><w:t>x%}", "{%x%}");
    check(r#"{{</w:t><w:t xml:space="preserve">v}}"#, "{{v}}");
    check(r"{#</w:t><w:t>c#}", "{#c#}");
    // Boundary: when there is no run boundary inside the tag, it is kept
    // verbatim.
    check(r"{%a%}", "{%a%}");
}

// ---- Step 3: colspan -------------------------------------------------------

#[test]
fn step3_colspan() {
    check(
        r#"<w:tc><w:tcPr><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t>{% colspan span %}</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:gridSpan w:val="{{span }}"/><w:tcW w:w="100"/></w:tcPr><w:p></w:p></w:tc>"#,
    );
    // gridSpan already present: remove the first old gridSpan and the empty
    // run carrying the tag, then inject.
    check(
        r#"<w:tc><w:tcPr><w:gridSpan w:val="2"/><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t></w:t></w:r><w:r><w:t>{% colspan 3 %}</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:gridSpan w:val="{{3 }}"/><w:tcW w:w="100"/></w:tcPr><w:p></w:p></w:tc>"#,
    );
    // Boundary: outside a w:tc, colspan has no effect.
    check(r"{% colspan x %}", "{% colspan x %}");
}

// ---- Step 4: cellbg --------------------------------------------------------

#[test]
fn step4_cellbg() {
    // The existing shd is removed and a new shd injected; the carrying run
    // becomes empty and is cleared.
    check(
        r#"<w:tc><w:tcPr><w:shd w:val="clear" w:fill="auto"/></w:tcPr><w:p><w:r><w:t>{% cellbg color %}</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="{{color }}"/></w:tcPr><w:p></w:p></w:tc>"#,
    );
    // When there is no shd, inject directly.
    check(
        r#"<w:tc><w:tcPr></w:tcPr><w:p><w:r><w:t>{% cellbg c %}</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:shd w:val="clear" w:color="auto" w:fill="{{c }}"/></w:tcPr><w:p></w:p></w:tc>"#,
    );
}

// ---- Step 5: xml:space="preserve" -----------------------------------------

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
    // Boundary: an attributed w:t is not annotated again.
    check(
        r#"<w:t xml:space="preserve">{{x}}</w:t>"#,
        r#"<w:t xml:space="preserve">{{x}}</w:t>"#,
    );
    // Boundary: a plain-text w:t without tags is unchanged.
    check(r"<w:t>hello</w:t>", r"<w:t>hello</w:t>");
}

// ---- Step 6: split runs on {{r }} / {%r %} (step 9 later promotes the
// middle run) ---------------------------------------------------------------

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
    // Boundary: whitespace after r is required; `{{rx}}` is only affected by
    // step 5.
    check(
        r"<w:t>{{rx}}</w:t>",
        r#"<w:t xml:space="preserve">{{rx}}</w:t>"#,
    );
}

// ---- Step 7: {%- left merge ------------------------------------------------

#[test]
fn step7_trim_left() {
    check(r"</w:t>FOO{%-bar%}", "{%bar%}");
    // Boundary: the guard must not cross another </w:t>; matching restarts at
    // the second </w:t>.
    check(r"</w:t>A</w:t>B{%-x%}", "</w:t>A{%x%}");
    // Boundary: unchanged when there is no `{%-`.
    check(r"</w:t>FOO{%bar%}", "</w:t>FOO{%bar%}");
}

// ---- Step 8: -%} right merge -----------------------------------------------

#[test]
fn step8_trim_right() {
    check(r"abc-%}XYZ<w:t>q", "abc%}q");
    check(r#"-%}x<w:t xml:space="preserve">y"#, "%}y");
    // Boundary: when {{ appears between -%} and <w:t>, the guard prevents
    // crossing and the whole span is unchanged.
    check(r"-%}a{{b}}<w:t>", "-%}a{{b}}<w:t>");
}

// ---- Step 9: tr/tc/p/r structured-tag promotion ----------------------------

#[test]
fn step9_structured_tags() {
    check(r"<w:p><w:r><w:t>{%p if %}</w:t></w:r></w:p>", "{% if %}");
    check(r"<w:p><w:r><w:t>{{p x}}</w:t></w:r></w:p>", "{{ x}}");
    check(
        r"<w:tr><w:tc><w:p><w:r><w:t>{%tr for%}</w:t></w:r></w:p></w:tc></w:tr>",
        "{% for%}",
    );
    // Boundary: ordinary tags (without the y prefix) are not promoted; step 5
    // only adds preserve.
    check(
        r"<w:p><w:r><w:t>{%x%}</w:t></w:r></w:p>",
        r#"<w:p><w:r><w:t xml:space="preserve">{%x%}</w:t></w:r></w:p>"#,
    );
}

// ---- Step 10: tr/tc/p comment structured-tag promotion ---------------------

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
    // Boundary: y=r gets no comment promotion.
    check(
        r"<w:r><w:t>{#r x #}</w:t></w:r>",
        r"<w:r><w:t>{#r x #}</w:t></w:r>",
    );
}

// ---- Step 11: vm vertical merge --------------------------------------------

#[test]
fn step11_v_merge() {
    check(
        r#"<w:tc><w:tcPr><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t>{% vm %}A</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:tcW w:w="100"/><w:vMerge w:val="{% if loop.first %}restart{% else %}continue{% endif %}"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">{% if loop.first %}A{% endif %}</w:t></w:r></w:p></w:tc>"#,
    );
    // Text on both sides of vm: g2+g3 is retained only on loop.first.
    check(
        r#"<w:tc><w:tcPr></w:tcPr><w:p><w:r><w:t>B{% vm %}A</w:t></w:r></w:p></w:tc>"#,
        r#"<w:tc><w:tcPr><w:vMerge w:val="{% if loop.first %}restart{% else %}continue{% endif %}"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">{% if loop.first %}BA{% endif %}</w:t></w:r></w:p></w:tc>"#,
    );
    // Boundary: vm outside a w:tc is not processed.
    check(r"{% vm %}", "{% vm %}");
}

// ---- Step 12: hm horizontal merge ------------------------------------------

#[test]
fn step12_h_merge_without_gridspan() {
    check(
        r#"<w:tc><w:tcPr><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t>{% hm %}A</w:t></w:r></w:p></w:tc>"#,
        r#"{% if loop.first %}<w:tc><w:tcPr><w:tcW w:w="100"/><w:gridSpan w:val="{{ loop.length }}"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc>{% endif %}"#,
    );
    // Boundary: hm outside a w:tc is not processed.
    check(r"{% hm %}", "{% hm %}");
}

#[test]
fn step12_h_merge_with_gridspan() {
    // gridSpan already present: multiply the value by loop.length, remove the
    // hm tag, and wrap the whole cell in if loop.first.
    check(
        r#"<w:tc><w:tcPr><w:gridSpan w:val="3"/><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t>{% hm %}A</w:t></w:r></w:p></w:tc>"#,
        r#"{% if loop.first %}<w:tc><w:tcPr><w:gridSpan w:val="{{ 3 * loop.length }}"/><w:tcW w:w="100"/></w:tcPr><w:p><w:r><w:t xml:space="preserve">A</w:t></w:r></w:p></w:tc>{% endif %}"#,
    );
}

// ---- Step 13: clean_tags ---------------------------------------------------

#[test]
fn step13_clean_tags() {
    check(r"{{a &lt; b &gt; c}}", "{{a < b > c}}");
    // Smart quotes (U+201C/U+201D) are restored to ASCII double quotes.
    check("{{x|default(\u{201c}N\u{201d})}}", r#"{{x|default("N")}}"#);
    // &#8216; left single quote entities are restored.
    check(r"{{&#8216;q&#8216;}}", "{{'q'}}");
}

#[test]
fn step13_clean_tags_scope_boundaries() {
    // Boundary: entities outside tags are untouched.
    check(r"a &lt; b", "a &lt; b");
    // Boundary: the inside of a {# #} comment is outside clean_tags' scope
    // (the lookbehind recognizes only {{ / {%).
    check(r"{# a &lt; b #}", "{# a &lt; b #}");
}

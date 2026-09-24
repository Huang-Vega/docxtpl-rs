#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Generate the Python-oracle differential-test fixtures.

Usage (from project root):

    python tests/fixtures/generate.py

Produces:
- tests/fixtures/templates/<id>.docx   python-docx built templates
- tests/fixtures/contexts/<id>.json    render fixtures only (roundtrip has none)
- tests/fixtures/manifest.json         fixture registry with upstream fingerprint

Idempotent: the templates/ and contexts/ directories are fully owned by this
script -- they are wiped and rewritten on every run.

Tag discipline (critical for oracle fidelity):
- Jinja tags in templates are written character-exact, spaces included,
  e.g. "{%p for i in items %}".
- Structured tags own their carrying element: a "{%p ... %}" tag occupies a
  paragraph whose single run text is exactly the tag; a "{%tr ... %}" tag sits
  in the first cell of its row while all other cells stay empty.

Note on `expected` in the manifest: it is the *measured* upstream status
(see tests/oracle/expected/report.json); fixtures whose measured status is
"error" are listed in EXPECTED_OVERRIDES below.
"""

import io
import json
import re
import shutil
import struct
import zipfile
import zlib
from pathlib import Path

from docx import Document
from docx.enum.section import WD_SECTION
from docx.opc.constants import RELATIONSHIP_TYPE as RT
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from docx.shared import Inches, Pt, RGBColor

SCRIPT_DIR = Path(__file__).resolve().parent
TEMPLATES_DIR = SCRIPT_DIR / "templates"
CONTEXTS_DIR = SCRIPT_DIR / "contexts"
MANIFEST_PATH = SCRIPT_DIR / "manifest.json"

UPSTREAM = {"docxtpl": "0.20.2", "sha": "cf5437bdf5d30f9362149ddea508d6d9f008b6cd"}

# Fixture ids whose measured upstream outcome is "error" (all others are "ok").
EXPECTED_OVERRIDES = {
    "r2_syntax_error": "error",
}

W_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
CT_CUSTOMXML = "application/vnd.openxmlformats-officedocument.customXmlProperties+xml"
REL_CUSTOMXML = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml"
CT_FOOTNOTES = "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"
REL_FOOTNOTES = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes"

FIXTURES = []


def fixture(fid, feature, phase, mode, context=None, post=None):
    def deco(build):
        FIXTURES.append({
            "id": fid, "feature": feature, "phase": phase, "mode": mode,
            "context": context, "post": post, "build": build,
        })
        return build
    return deco


# ---------------------------------------------------------------------------
# generic helpers
# ---------------------------------------------------------------------------

def minimal_png():
    """Hand-built 1x1 red-pixel PNG (zlib + struct, no PIL involved)."""

    def chunk(ctype, data):
        blob = ctype + data
        return (struct.pack(">I", len(data)) + blob
                + struct.pack(">I", zlib.crc32(blob) & 0xFFFFFFFF))

    ihdr = struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr)
            + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00"))
            + chunk(b"IEND", b""))


def _insert_before_closing(xml_bytes, closing, fragment):
    text = xml_bytes.decode("utf-8")
    idx = text.rindex(closing)
    return (text[:idx] + fragment + text[idx:]).encode("utf-8")


def _next_rid(rels_bytes):
    ids = [int(m) for m in re.findall(r'Id="rId(\d+)"', rels_bytes.decode("utf-8"))]
    return "rId%d" % (max(ids) + 1 if ids else 1)


def _add_content_type_override(part_name, content_type):
    def transform(data):
        frag = '<Override PartName="%s" ContentType="%s"/>' % (part_name, content_type)
        return _insert_before_closing(data, "</Types>", frag)
    return transform


def _add_relationship(rel_type, target):
    def transform(data):
        frag = '<Relationship Id="%s" Type="%s" Target="%s"/>' % (
            _next_rid(data), rel_type, target)
        return _insert_before_closing(data, "</Relationships>", frag)
    return transform


def _append_footnote_reference(data):
    from lxml import etree
    root = etree.fromstring(data)
    body = root.find("{%s}body" % W_NS)
    first_p = body.find("{%s}p" % W_NS)
    run = etree.SubElement(first_p, "{%s}r" % W_NS)
    run.append(etree.fromstring('<w:footnoteReference xmlns:w="%s" w:id="1"/>' % W_NS))
    return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def _zipinfo(name):
    info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
    info.external_attr = 0o600 << 16
    return info


def rewrite_zip(path, add_parts=None, transform=None):
    """Rewrite a docx zip in place: add parts and/or transform existing ones.

    Untouched entries are copied byte-identically; new entries are deflated.
    """
    add_parts = add_parts or {}
    transform = transform or {}
    entries = []
    with zipfile.ZipFile(path) as zin:
        for info in zin.infolist():
            if info.filename.endswith("/"):
                continue
            entries.append((info.filename, zin.read(info.filename), info.compress_type))
    tmp = path.with_name(path.name + ".tmp")
    with zipfile.ZipFile(tmp, "w") as zout:
        for name, data, compress in entries:
            if name in transform:
                data = transform[name](data)
            zout.writestr(_zipinfo(name), data, compress_type=compress)
        for name in sorted(add_parts):
            zout.writestr(_zipinfo(name), add_parts[name],
                          compress_type=zipfile.ZIP_DEFLATED)
    tmp.replace(path)


CUSTOM_XML = (
    "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n"
    '<customItem xmlns="http://schemas.example.com/docxtplrs-custom">'
    '<name>item1</name><value>custom data</value></customItem>'
).encode("utf-8")

FOOTNOTES_XML = (
    "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n"
    '<w:footnotes xmlns:w="' + W_NS + '">'
    '<w:footnote w:type="separator" w:id="-1">'
    "<w:p><w:r><w:separator/></w:r></w:p></w:footnote>"
    '<w:footnote w:type="continuationSeparator" w:id="0">'
    "<w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>"
    '<w:footnote w:id="1"><w:p><w:r><w:t>FN text</w:t></w:r></w:p></w:footnote>'
    "</w:footnotes>"
).encode("utf-8")


def postprocess_unknown_part(path):
    # NB: the python-docx default template already ships customXml/item1.xml
    # (+ itemProps1.xml and its rels), so the genuinely "unknown" part added
    # here is item2.xml.
    rewrite_zip(
        path,
        add_parts={"customXml/item2.xml": CUSTOM_XML},
        transform={
            "[Content_Types].xml": _add_content_type_override("/customXml/item2.xml", CT_CUSTOMXML),
            "word/_rels/document.xml.rels": _add_relationship(REL_CUSTOMXML, "../customXml/item2.xml"),
        },
    )


def postprocess_footnotes(path):
    rewrite_zip(
        path,
        add_parts={"word/footnotes.xml": FOOTNOTES_XML},
        transform={
            "[Content_Types].xml": _add_content_type_override("/word/footnotes.xml", CT_FOOTNOTES),
            "word/_rels/document.xml.rels": _add_relationship(REL_FOOTNOTES, "footnotes.xml"),
            "word/document.xml": _append_footnote_reference,
        },
    )


# ===========================================================================
# P1 -- roundtrip fixtures (mode=roundtrip, no jinja tags)
# ===========================================================================

# 1
@fixture("rt_minimal", "roundtrip: minimal single-paragraph document", "P1", "roundtrip")
def rt_minimal(doc):
    doc.add_paragraph("Hello World")


# 2
@fixture("rt_paragraphs", "roundtrip: multiple plain paragraphs", "P1", "roundtrip")
def rt_paragraphs(doc):
    for text in [
        "The quick brown fox jumps over the lazy dog.",
        "Pack my box with five dozen liquor jugs.",
        "How vexingly quick daft zebras jump!",
        "Sphinx of black quartz, judge my vow.",
        "The five boxing wizards jump quickly.",
    ]:
        doc.add_paragraph(text)


# 3
@fixture("rt_special_chars", "roundtrip: XML special characters in text", "P1", "roundtrip")
def rt_special_chars(doc):
    doc.add_paragraph("AT&T \"quoted\" <tag> 'apostrophe' 5 > 3")


# 4
@fixture("rt_formats", "roundtrip: run formatting (bold/italic/underline/color/size)", "P1", "roundtrip")
def rt_formats(doc):
    p = doc.add_paragraph()
    p.add_run("bold ").bold = True
    p.add_run("italic ").italic = True
    p.add_run("underline ").underline = True
    red = p.add_run("red ")
    red.font.color.rgb = RGBColor(0xFF, 0x00, 0x00)
    p.add_run("size16 ").font.size = Pt(16)
    p.add_run("plain")


# 5
@fixture("rt_styles", "roundtrip: paragraph styles and character style", "P1", "roundtrip")
def rt_styles(doc):
    doc.add_paragraph("Heading One", style="Heading 1")
    doc.add_paragraph("Heading Two", style="Heading 2")
    p = doc.add_paragraph("This has a ")
    char_run = p.add_run("char style")
    char_run.style = doc.styles["Emphasis"]
    p.add_run(" in it.")


# 6
@fixture("rt_lists", "roundtrip: bullet and numbered list styles", "P1", "roundtrip")
def rt_lists(doc):
    for i in range(1, 4):
        doc.add_paragraph("Bullet %d" % i, style="List Bullet")
    for i in range(1, 4):
        doc.add_paragraph("Numbered %d" % i, style="List Number")


# 7
@fixture("rt_table_basic", "roundtrip: plain table with one empty cell", "P1", "roundtrip")
def rt_table_basic(doc):
    t = doc.add_table(rows=3, cols=3)
    for r in range(3):
        for c in range(3):
            if (r, c) == (1, 1):
                continue  # cell (1,1) stays empty
            t.cell(r, c).text = "R%dC%d" % (r, c)


# 8
@fixture("rt_table_merged", "roundtrip: horizontal and vertical cell merge", "P1", "roundtrip")
def rt_table_merged(doc):
    t = doc.add_table(rows=4, cols=3)
    t.cell(0, 0).merge(t.cell(0, 1))  # horizontal merge in row 0
    t.cell(1, 0).merge(t.cell(2, 0))  # vertical merge in columns 0-1


# 9
@fixture("rt_nested_table", "roundtrip: table nested inside a cell", "P1", "roundtrip")
def rt_nested_table(doc):
    t = doc.add_table(rows=2, cols=2)
    t.cell(0, 0).add_table(rows=1, cols=2)


# 10
@fixture("rt_xml_space", "roundtrip: whitespace runs and xml:space", "P1", "roundtrip")
def rt_xml_space(doc):
    p = doc.add_paragraph()
    p.add_run("  lead")
    p.add_run("mid  dle")
    p.add_run("trail  ")


# 11
@fixture("rt_media", "roundtrip: embedded image (hand-built minimal PNG)", "P1", "roundtrip")
def rt_media(doc):
    doc.add_picture(io.BytesIO(minimal_png()), width=Inches(1))


# 12
@fixture("rt_header_footer", "roundtrip: header and footer parts", "P1", "roundtrip")
def rt_header_footer(doc):
    section = doc.sections[0]
    header = section.header
    header.is_linked_to_previous = False
    header.paragraphs[0].text = "HDR {{x}}"  # roundtrip: tag stays verbatim
    footer = section.footer
    footer.is_linked_to_previous = False
    footer.paragraphs[0].text = "FTR"


# 13
@fixture("rt_hyperlink", "roundtrip: external hyperlink relationship", "P1", "roundtrip")
def rt_hyperlink(doc):
    p = doc.add_paragraph()
    r_id = doc.part.relate_to("https://example.com", RT.HYPERLINK, is_external=True)
    hyperlink = OxmlElement("w:hyperlink")
    hyperlink.set(qn("r:id"), r_id)
    run = OxmlElement("w:r")
    t = OxmlElement("w:t")
    t.text = "Example Link"
    run.append(t)
    hyperlink.append(run)
    p._p.append(hyperlink)


# 14
@fixture("rt_bookmark", "roundtrip: bookmark and PAGE field", "P1", "roundtrip")
def rt_bookmark(doc):
    p1 = doc.add_paragraph("Bookmarked text")
    start = OxmlElement("w:bookmarkStart")
    start.set(qn("w:id"), "0")
    start.set(qn("w:name"), "BM1")
    end = OxmlElement("w:bookmarkEnd")
    end.set(qn("w:id"), "0")
    p1._p.insert(0, start)
    p1._p.append(end)
    p2 = doc.add_paragraph()
    fld = OxmlElement("w:fldSimple")
    fld.set(qn("w:instr"), "PAGE")
    run = OxmlElement("w:r")
    t = OxmlElement("w:t")
    t.text = "1"
    run.append(t)
    fld.append(run)
    p2._p.append(fld)


# 15
@fixture("rt_unknown_part", "roundtrip: unknown part + content type + rel", "P1", "roundtrip",
         post=postprocess_unknown_part)
def rt_unknown_part(doc):
    doc.add_paragraph("Unknown part body")


# 16
@fixture("rt_unicode", "roundtrip: unicode text (CJK/emoji/accented/Cyrillic)", "P1", "roundtrip")
def rt_unicode(doc):
    doc.add_paragraph("中文测试 🎉 éàü комбинация")


# 17
@fixture("rt_long", "roundtrip: 100 paragraphs", "P1", "roundtrip")
def rt_long(doc):
    for i in range(1, 101):
        doc.add_paragraph("Paragraph %d" % i)


# 18
@fixture("rt_footnotes", "roundtrip: footnotes part and footnote reference", "P1", "roundtrip",
         post=postprocess_footnotes)
def rt_footnotes(doc):
    doc.add_paragraph("Main text with footnote")


# 19
@fixture("rt_rsid", "roundtrip: rsid attributes (Word noise)", "P1", "roundtrip")
def rt_rsid(doc):
    p = doc.add_paragraph("Rsid noise test")
    p._p.set(qn("w:rsidR"), "00A1B2C3")
    p._p.set(qn("w:rsidRDefault"), "00A1B2C4")
    p._p.set(qn("w:rsidP"), "00D4E5F6")


# 20
@fixture("rt_multi_section", "roundtrip: multiple sections", "P1", "roundtrip")
def rt_multi_section(doc):
    doc.add_paragraph("Section one text")
    doc.add_section(WD_SECTION.NEW_PAGE)
    doc.add_paragraph("Section two text")


# ===========================================================================
# P2 -- render fixtures (mode=render, inline jinja)
# ===========================================================================

# 21
@fixture("r2_var_basic", "render: basic variable", "P2", "render", context={"name": "World"})
def r2_var_basic(doc):
    doc.add_paragraph("Hello {{name}}!")


# 22
@fixture("r2_var_multiple", "render: multiple variables", "P2", "render",
         context={"greeting": "Hi", "name": "Vega"})
def r2_var_multiple(doc):
    doc.add_paragraph("{{greeting}}, {{name}}.")


# 23
@fixture("r2_var_spacing", "render: padded tag delimiters", "P2", "render", context={"name": "X"})
def r2_var_spacing(doc):
    doc.add_paragraph("Hello {{ name }} !")


# 24
@fixture("r2_filter_upper", "render: upper filter", "P2", "render", context={"name": "abc"})
def r2_filter_upper(doc):
    doc.add_paragraph("{{name|upper}}")


# 25
@fixture("r2_filter_join", "render: join filter", "P2", "render",
         context={"items": ["a", "b", "c"]})
def r2_filter_join(doc):
    doc.add_paragraph("{{items|join(', ')}}")


# 26
@fixture("r2_filter_default", "render: default filter", "P2", "render", context={})
def r2_filter_default(doc):
    doc.add_paragraph("{{missing|default('fb')}}")


# 27
@fixture("r2_undefined", "render: undefined variable", "P2", "render", context={})
def r2_undefined(doc):
    doc.add_paragraph("Hi {{missing}}!")


# 28
@fixture("r2_value_amp", "render: value with ampersand (entity break)", "P2", "render",
         context={"name": "AT&T Inc"})
def r2_value_amp(doc):
    doc.add_paragraph("Company: {{name}}")


# 29
@fixture("r2_value_specials", "render: value with special characters", "P2", "render",
         context={"text": "R&D > 50% \"q\" 'a'"})
def r2_value_specials(doc):
    doc.add_paragraph("V: {{text}}")


# 30
@fixture("r2_value_lt_tag", "render: value forming well-formed tags", "P2", "render",
         context={"text": "a<b>c</b>d"})
def r2_value_lt_tag(doc):
    doc.add_paragraph("V: {{text}}")


# 31
@fixture("r2_value_lt_bare", "render: value with bare < (malformed XML)", "P2", "render",
         context={"text": "3<5 x<y"})
def r2_value_lt_bare(doc):
    doc.add_paragraph("V: {{text}}")


# 32
@fixture("r2_if_inline_true", "render: inline if/else, true branch", "P2", "render",
         context={"show": True})
def r2_if_inline_true(doc):
    doc.add_paragraph("{% if show %}YES{% else %}NO{% endif %}")


# 33
@fixture("r2_if_inline_false", "render: inline if/else, false branch", "P2", "render",
         context={"show": False})
def r2_if_inline_false(doc):
    doc.add_paragraph("{% if show %}YES{% else %}NO{% endif %}")


# 34
@fixture("r2_if_cross_runs", "render: inline if split across runs", "P2", "render",
         context={"x": True})
def r2_if_cross_runs(doc):
    p = doc.add_paragraph()
    p.add_run("A {% if x %}")
    bold = p.add_run("B")
    bold.bold = True
    p.add_run("{% endif %} C")


# 35
@fixture("r2_for_inline", "render: inline for loop", "P2", "render",
         context={"items": [1, 2, 3]})
def r2_for_inline(doc):
    doc.add_paragraph("{% for i in items %}[{{i}}]{% endfor %}")


# 36
@fixture("r2_for_cross_runs", "render: for loop split across runs", "P2", "render",
         context={"items": [1, 2, 3]})
def r2_for_cross_runs(doc):
    p = doc.add_paragraph()
    p.add_run("{% for i in items %}")
    p.add_run("{{i}};")
    p.add_run("{% endfor %}")


# 37
@fixture("r2_trim", "render: whitespace-control trim markers", "P2", "render",
         context={"x": True})
def r2_trim(doc):
    doc.add_paragraph("Before")
    doc.add_paragraph("X{%- if x -%}Y{%- endif -%}Z")
    doc.add_paragraph("After")


# 38
@fixture("r2_comment", "render: jinja comment", "P2", "render", context={})
def r2_comment(doc):
    doc.add_paragraph("a{# note #}b")


# 39
@fixture("r2_split_delims", "render: delimiters split across runs", "P2", "render",
         context={"name": "Merged"})
def r2_split_delims(doc):
    p = doc.add_paragraph()
    p.add_run("Split{")
    p.add_run("{name}}")


# 40
@fixture("r2_split_content", "render: tag content split across runs", "P2", "render",
         context={"name": "Spliced"})
def r2_split_content(doc):
    p = doc.add_paragraph()
    p.add_run("{{na")
    p.add_run("me}}")


# 41
@fixture("r2_literal_escape", "render: underscored escape {_{ }_}", "P2", "render", context={})
def r2_literal_escape(doc):
    doc.add_paragraph("literal {_{x}_} stays")


# 42
@fixture("r2_newline_value", "render: newline in value", "P2", "render",
         context={"text": "l1\nl2"})
def r2_newline_value(doc):
    doc.add_paragraph("V: {{text}}")


# 43
@fixture("r2_tab_value", "render: tab in value", "P2", "render", context={"text": "a\tb"})
def r2_tab_value(doc):
    doc.add_paragraph("V: {{text}}")


# 44
@fixture("r2_smart_quotes", "render: smart quotes inside tag", "P2", "render", context={})
def r2_smart_quotes(doc):
    doc.add_paragraph("{{name|default(“N/A”)}}")


# 45
@fixture("r2_entity_in_tag", "render: > entity inside tag", "P2", "render",
         context={"a": 2, "b": 1})
def r2_entity_in_tag(doc):
    doc.add_paragraph("{% if a > b %}A{% else %}B{% endif %}")


# 46
@fixture("r2_syntax_error", "render: jinja syntax error (missing endif)", "P2", "render",
         context={})
def r2_syntax_error(doc):
    doc.add_paragraph("{% if x %}open")


# 47
@fixture("r2_for_empty", "render: for loop over empty list", "P2", "render",
         context={"items": []})
def r2_for_empty(doc):
    doc.add_paragraph("{% for i in items %}[{{i}}]{% endfor %}")


# ===========================================================================
# P3 -- render fixtures (mode=render, structured tags own their element)
# ===========================================================================

def fill_row(table, row, texts):
    for col, text in enumerate(texts):
        table.cell(row, col).text = text


# 48
@fixture("r3_p_if_true", "render: {%p %} paragraph if, true", "P3", "render",
         context={"show": True})
def r3_p_if_true(doc):
    doc.add_paragraph("Before")
    doc.add_paragraph("{%p if show %}")
    doc.add_paragraph("Middle")
    doc.add_paragraph("{%p endif %}")
    doc.add_paragraph("After")


# 49
@fixture("r3_p_if_false", "render: {%p %} paragraph if, false", "P3", "render",
         context={"show": False})
def r3_p_if_false(doc):
    doc.add_paragraph("Before")
    doc.add_paragraph("{%p if show %}")
    doc.add_paragraph("Middle")
    doc.add_paragraph("{%p endif %}")
    doc.add_paragraph("After")


# 50
@fixture("r3_p_for", "render: {%p %} paragraph for loop", "P3", "render",
         context={"items": [1, 2, 3]})
def r3_p_for(doc):
    doc.add_paragraph("{%p for i in items %}")
    doc.add_paragraph("Item {{i}}")
    doc.add_paragraph("{%p endfor %}")


# 51
@fixture("r3_p_for_empty", "render: {%p %} loop over empty list", "P3", "render",
         context={"items": []})
def r3_p_for_empty(doc):
    doc.add_paragraph("{%p for i in items %}")
    doc.add_paragraph("Item {{i}}")
    doc.add_paragraph("{%p endfor %}")


# 52
@fixture("r3_p_nested", "render: nested {%p %} loops", "P3", "render",
         context={"groups": [{"n": 1, "subs": ["a", "b"]}, {"n": 2, "subs": ["c"]}]})
def r3_p_nested(doc):
    doc.add_paragraph("{%p for g in groups %}")
    doc.add_paragraph("{%p for s in g.subs %}")
    doc.add_paragraph("G={{g.n}} S={{s}}")
    doc.add_paragraph("{%p endfor %}")
    doc.add_paragraph("{%p endfor %}")


# 53
@fixture("r3_p_set", "render: {%p set %}", "P3", "render", context={})
def r3_p_set(doc):
    doc.add_paragraph("{%p set x = 40 + 2 %}")
    doc.add_paragraph("x is {{x}}")


# 54
@fixture("r3_tr_for", "render: {%tr %} table row for loop", "P3", "render",
         context={"rows": [{"a": "1", "b": "2"}, {"a": "3", "b": "4"}]})
def r3_tr_for(doc):
    t = doc.add_table(rows=4, cols=2)
    fill_row(t, 0, ["H1", "H2"])
    t.cell(1, 0).text = "{%tr for r in rows %}"  # other cells stay empty
    fill_row(t, 2, ["{{r.a}}", "{{r.b}}"])
    t.cell(3, 0).text = "{%tr endfor %}"


# 55
@fixture("r3_tr_for_empty", "render: {%tr %} loop over empty rows", "P3", "render",
         context={"rows": []})
def r3_tr_for_empty(doc):
    t = doc.add_table(rows=4, cols=2)
    fill_row(t, 0, ["H1", "H2"])
    t.cell(1, 0).text = "{%tr for r in rows %}"
    fill_row(t, 2, ["{{r.a}}", "{{r.b}}"])
    t.cell(3, 0).text = "{%tr endfor %}"


# 56
@fixture("r3_tr_if", "render: {%tr %} row if", "P3", "render", context={"show": True})
def r3_tr_if(doc):
    t = doc.add_table(rows=4, cols=2)
    fill_row(t, 0, ["H1", "H2"])
    t.cell(1, 0).text = "{%tr if show %}"
    fill_row(t, 2, ["A", "B"])
    t.cell(3, 0).text = "{%tr endif %}"


# 57
@fixture("r3_tr_nested_p", "render: {%p %} nested inside {%tr %} row", "P3", "render",
         context={"rows": [{"tags": ["x", "y"], "n": 1}, {"tags": ["z"], "n": 2}]})
def r3_tr_nested_p(doc):
    t = doc.add_table(rows=4, cols=2)
    fill_row(t, 0, ["H1", "H2"])
    t.cell(1, 0).text = "{%tr for r in rows %}"
    cell = t.cell(2, 0)
    cell.text = "{%p for t in r.tags %}"
    cell.add_paragraph("{{t}}")
    cell.add_paragraph("{%p endfor %}")
    t.cell(2, 1).text = "{{r.n}}"
    t.cell(3, 0).text = "{%tr endfor %}"


# 58
@fixture("r3_tc_for", "render: {%tc %} cell for loop", "P3", "render",
         context={"cols": ["A", "B", "C"]})
def r3_tc_for(doc):
    t = doc.add_table(rows=1, cols=3)
    t.cell(0, 0).text = "{%tc for c in cols %}"
    t.cell(0, 1).text = "{{c}}"
    t.cell(0, 2).text = "{%tc endfor %}"


# 59
@fixture("r3_nested_tables", "render: {%tr %} loops across nested tables", "P3", "render",
         context={"groups": [{"subs": ["a", "b"]}, {"subs": ["c"]}]})
def r3_nested_tables(doc):
    outer = doc.add_table(rows=4, cols=1)
    outer.cell(0, 0).text = "Header"
    outer.cell(1, 0).text = "{%tr for g in groups %}"
    inner = outer.cell(2, 0).add_table(rows=3, cols=1)
    inner.cell(0, 0).text = "{%tr for s in g.subs %}"
    inner.cell(1, 0).text = "{{s}}"
    inner.cell(2, 0).text = "{%tr endfor %}"
    outer.cell(3, 0).text = "{%tr endfor %}"


# 60
@fixture("r3_r_tag", "render: {%r %} run-level if/else", "P3", "render",
         context={"bold": True})
def r3_r_tag(doc):
    p = doc.add_paragraph()
    p.add_run("{%r if bold %}")
    bold = p.add_run("Bold text")
    bold.bold = True
    p.add_run("{%r else %}")
    p.add_run("Plain")
    p.add_run("{%r endif %}")


# 61
@fixture("r3_p_var", "render: {{p ...}} paragraph-variable syntax", "P3", "render",
         context={"name": "Floating"})
def r3_p_var(doc):
    doc.add_paragraph("{{p name}}")


# 62
@fixture("r3_colspan", "render: {% colspan %} cell merge tag", "P3", "render",
         context={"span": 2})
def r3_colspan(doc):
    t = doc.add_table(rows=2, cols=2)
    t.cell(0, 0).text = "{% colspan span %}"


# 63
@fixture("r3_cellbg", "render: {% cellbg %} cell background tag", "P3", "render",
         context={"color": "FF0000"})
def r3_cellbg(doc):
    t = doc.add_table(rows=2, cols=2)
    t.cell(0, 0).text = "{% cellbg color %}"


# 64
@fixture("r3_vm", "render: {%vm%} vertical merge", "P3", "render",
         context={"rows": [{"k": "a"}, {"k": "b"}, {"k": "c"}]})
def r3_vm(doc):
    t = doc.add_table(rows=4, cols=2)
    fill_row(t, 0, ["Cat", "Val"])
    t.cell(1, 0).text = "{%tr for r in rows %}"
    t.cell(2, 0).text = "V{%vm%}X"
    t.cell(2, 1).text = "{{r.k}}"
    t.cell(3, 0).text = "{%tr endfor %}"


# 65
@fixture("r3_hm", "render: {%hm%} horizontal merge", "P3", "render",
         context={"rows": [1, 2]})
def r3_hm(doc):
    t = doc.add_table(rows=4, cols=3)
    fill_row(t, 0, ["H1", "H2", "H3"])
    t.cell(1, 0).text = "{%tr for r in rows %}"
    t.cell(2, 0).text = "{%hm%}Merged"
    t.cell(2, 1).text = "X"
    t.cell(2, 2).text = "X"
    t.cell(3, 0).text = "{%tr endfor %}"


# 66
@fixture("r3_fix_add", "render: table with missing gridCol (fix_tables input)", "P3", "render",
         context={})
def r3_fix_add(doc):
    t = doc.add_table(rows=2, cols=3)
    grid = t._tbl.tblGrid
    last_col = grid.findall(qn("w:gridCol"))[-1]
    grid.remove(last_col)  # 3 columns become 2 in the grid, cells stay 3


# 67
@fixture("r3_fix_remove", "render: table with gridSpan merges (fix_tables input)", "P3", "render",
         context={})
def r3_fix_remove(doc):
    t = doc.add_table(rows=2, cols=3)
    t.cell(0, 0).merge(t.cell(0, 1))  # gridSpan=2 in row 0
    t.cell(1, 0).merge(t.cell(1, 1))  # gridSpan=2 in row 1


# 68
@fixture("r3_docpr", "render: drawing/docPr with variable", "P3", "render",
         context={"name": "pic"})
def r3_docpr(doc):
    doc.add_picture(io.BytesIO(minimal_png()))
    doc.add_paragraph("N: {{name}}")


# 69
@fixture("r3_combo_invoice", "render: combined invoice (title + {%tr %} + {%p %})", "P3", "render",
         context={"num": "2026-001",
                  "items": [{"name": "A", "qty": 1, "price": "9.9"},
                            {"name": "B", "qty": 2, "price": "5.5"}],
                  "notes": ["n1", "n2"]})
def r3_combo_invoice(doc):
    doc.add_paragraph("Invoice {{num}}")
    t = doc.add_table(rows=4, cols=3)
    fill_row(t, 0, ["Item", "Qty", "Price"])
    t.cell(1, 0).text = "{%tr for it in items %}"
    fill_row(t, 2, ["{{it.name}}", "{{it.qty}}", "{{it.price}}"])
    t.cell(3, 0).text = "{%tr endfor %}"
    doc.add_paragraph("{%p for n in notes %}")
    doc.add_paragraph("Note: {{n}}")
    doc.add_paragraph("{%p endfor %}")


# ===========================================================================
# main
# ===========================================================================

def main():
    for directory in (TEMPLATES_DIR, CONTEXTS_DIR):
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True, exist_ok=True)

    records = []
    for fx in FIXTURES:
        doc = Document()
        fx["build"](doc)
        template_path = TEMPLATES_DIR / (fx["id"] + ".docx")
        doc.save(template_path)
        if fx["post"] is not None:
            fx["post"](template_path)

        context_field = None
        if fx["mode"] == "render":
            context_path = CONTEXTS_DIR / (fx["id"] + ".json")
            context_path.write_text(
                json.dumps(fx["context"], indent=2, ensure_ascii=False) + "\n",
                encoding="utf-8")
            context_field = "contexts/%s.json" % fx["id"]

        records.append({
            "id": fx["id"],
            "feature": fx["feature"],
            "phase": fx["phase"],
            "mode": fx["mode"],
            "template": "templates/%s.docx" % fx["id"],
            "context": context_field,
            "expected": EXPECTED_OVERRIDES.get(fx["id"], "ok"),
            "allowed_normalizations": [],
            "known_deviations": [],
            "owner": "vegah",
            "issue": "",
        })

    manifest = {"upstream": UPSTREAM, "fixtures": records}
    MANIFEST_PATH.write_text(
        json.dumps(manifest, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8")

    context_count = sum(1 for r in records if r["context"])
    print("generated %d fixtures -> %s" % (len(records), MANIFEST_PATH))
    print("  templates: %d (%s)" % (len(records), TEMPLATES_DIR))
    print("  contexts : %d (%s)" % (context_count, CONTEXTS_DIR))


if __name__ == "__main__":
    main()

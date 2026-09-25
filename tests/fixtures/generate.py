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
MEDIA_DIR = SCRIPT_DIR / "media"
MANIFEST_PATH = SCRIPT_DIR / "manifest.json"

UPSTREAM = {"docxtpl": "0.20.2", "sha": "cf5437bdf5d30f9362149ddea508d6d9f008b6cd"}

# Fixture ids whose measured upstream outcome is "error" (all others are "ok").
EXPECTED_OVERRIDES = {
    "r2_syntax_error": "error",
    "p4_img_bad": "error",
}

W_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
CT_CUSTOMXML = "application/vnd.openxmlformats-officedocument.customXmlProperties+xml"
REL_CUSTOMXML = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml"
CT_FOOTNOTES = "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"
REL_FOOTNOTES = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes"

FIXTURES = []


def fixture(fid, feature, phase, mode, context=None, post=None,
            context_kind="json", context_src=None):
    def deco(build):
        FIXTURES.append({
            "id": fid, "feature": feature, "phase": phase, "mode": mode,
            "context": context, "post": post, "build": build,
            "context_kind": context_kind, "context_src": context_src,
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
# P4 -- media samples (hand-built, deterministic; MEDIA_DIR is script-owned)
# ===========================================================================

def _png_chunk(ctype, data):
    blob = ctype + data
    return (struct.pack(">I", len(data)) + blob
            + struct.pack(">I", zlib.crc32(blob) & 0xFFFFFFFF))


def make_png(w, h, px_per_unit=None):
    """RGB PNG；px_per_unit 为 pHYs 像素/米（None 则省略 pHYs，dpi=72）。"""
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    raw = b"".join(b"\x00" + b"\x00\x7f\xff" * w for _ in range(h))
    out = b"\x89PNG\r\n\x1a\n" + _png_chunk(b"IHDR", ihdr)
    if px_per_unit is not None:
        out += _png_chunk(b"pHYs", struct.pack(">IIB", px_per_unit, px_per_unit, 1))
    out += _png_chunk(b"IDAT", zlib.compress(raw))
    return out + _png_chunk(b"IEND", b"")


def make_jpeg(w, h, dpi):
    """最小 JFIF（APP0 dpi + SOF0），不被解码，仅头有效。

    JFIF 段布局：marker(FF E0) + len + 'JFIF\\0' + ver(01 02) + units + x/y + 缩略图尺寸。
    units=1 表示英寸（python-docx 依此把 density 当 dpi；0 会退化成 72dpi）。
    """
    jpg = b"\xff\xd8"
    jpg += (b"\xff\xe0" + struct.pack(">H", 16) + b"JFIF\x00\x01\x02\x01"
            + struct.pack(">HH", dpi, dpi) + b"\x00\x00")
    jpg += (b"\xff\xc0" + struct.pack(">H", 17) + b"\x08"
            + struct.pack(">HH", h, w) + b"\x03" + b"\x01\x11\x00" * 3)
    return jpg + b"\xff\xd9"


def make_bmp(w, h, px_per_m):
    """24bpp BITMAPINFOHEADER BMP（bottom-up，行 4 字节对齐）。"""
    row = ((w * 3 + 3) // 4) * 4
    pix = b"\x00" * (row * h)
    dib = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(pix),
                      px_per_m, px_per_m, 0, 0)
    off = 14 + 40
    fh = struct.pack("<2sIHHI", b"BM", off + len(pix), 0, 0, off)
    return fh + dib + pix


def make_gif(w, h):
    """GIF87a（2 色全局色表，LZW 数据仅头有效，不被解码）。"""
    return (b"GIF87a" + struct.pack("<HHBBB", w, h, 0x80, 0, 0)
            + b"\x00\x00\x00\xff\xff\xff"
            + b"," + struct.pack("<HHHHB", 0, 0, w, h, 0)
            + b"\x02\x02\x2c\x01;"
            )


def make_tiff(w, h, res_num, unit=2):
    """小端 TIFF：IFD0 含尺寸与 RATIONAL 分辨率（值区在 IFD 之后）。"""
    res_off = 8 + 2 + 12 * 5 + 4
    entries = [
        (256, 4, 1, w),           # ImageWidth LONG
        (257, 4, 1, h),           # ImageLength LONG
        (282, 5, 1, res_off),     # XResolution RATIONAL
        (283, 5, 1, res_off),     # YResolution RATIONAL
        (296, 3, 1, unit),        # ResolutionUnit SHORT（值内联）
    ]
    ifd = b"".join(struct.pack("<HHII", t, ty, c, v) for t, ty, c, v in entries)
    return (b"II*\x00" + struct.pack("<I", 8) + struct.pack("<H", len(entries))
            + ifd + struct.pack("<I", 0) + struct.pack("<II", res_num, 1))


MEDIA_FILES = {
    # 2x1 PNG，无 pHYs -> 72dpi -> EMU 25400x12700
    "p4_dot2x1.png": lambda: make_png(2, 1),
    # 4x1 PNG，pHYs 5906 px/m -> int(round(5906*0.0254))=150dpi -> 24384x6096
    "p4_wide4x1.png": lambda: make_png(4, 1, px_per_unit=5906),
    # 4x2 JFIF，300dpi -> native EMU 12192x6096
    "p4_rect4x2.jpg": lambda: make_jpeg(4, 2, 300),
    # 4x2 BMP，2835 px/m -> round(71.889)=72dpi
    "p4_brick4x2.bmp": lambda: make_bmp(4, 2, 2835),
    "p4_arrow4x2.gif": lambda: make_gif(4, 2),
    # 4x2 TIFF，150/1 dpi，unit=2(英寸)
    "p4_tile4x2.tiff": lambda: make_tiff(4, 2, 150),
    # 非图片：用于 UnrecognizedImageError
    "p4_bad.png": lambda: b"this is not an image at all\n",
}


# ===========================================================================
# P4 -- render fixtures (mode=render, typed context via build_context(tpl))
# ===========================================================================

_CTX_HEADER = '''\
# -*- coding: utf-8 -*-
"""P4 fixture context（由 tests/fixtures/generate.py 生成，勿手改）。

提供 build_context(tpl)：返回渲染上下文，可包含 RichText/RichTextParagraph/
Listing/InlineImage 等类型化值（对齐上游 docxtpl 0.20.2 用法）。
"""
import os

from docx.shared import Mm
from docxtpl import InlineImage, Listing, RichText, RichTextParagraph

_HERE = os.path.dirname(os.path.abspath(__file__))


def _img(name):
    return os.path.join(_HERE, os.pardir, "media", name)

'''


# 70
@fixture("p4_rt_basic", "render: RichText props + concat + empty", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    rt = RichText("中文<b>粗", bold=True, color="#FF0000", size="20")
    rt.add("斜体", italic=True)
    rt.add("下划线", underline=True)
    rt.add("删除线", strike=True)
    rt.add("高亮", highlight="#FFFF00")
    return {"rt": rt, "empty": RichText()}
''')
def p4_rt_basic(doc):
    doc.add_paragraph("A: {{rt}} B: {{empty}}E")


# 71
@fixture("p4_rt_style_font", "render: RichText style/font/sub/sup/lang/rtl", "P4",
         "render", context_kind="python",
         context_src='''
def build_context(tpl):
    rt1 = RichText("样式", style="Emphasis")
    rt1.add("区域字体", font="eastAsia:SimSun")
    rt2 = RichText("上标", superscript=True, lang="zh-CN")
    rt3 = RichText("RTL", bold=True, rtl=True)
    rt3.add("下标", subscript=True)
    return {"rt1": rt1, "rt2": rt2, "rt3": rt3}
''')
def p4_rt_style_font(doc):
    doc.add_paragraph("X{{rt1}}Y{{rt2}}Z{{rt3}}")


# 72
@fixture("p4_rt_url", "render: RichText hyperlink via url_id", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    url_id = tpl.build_url_id("https://docxtpl.readthedocs.io/")
    return {"rt": RichText("链接文本", url_id=url_id, color="0563C1",
                           underline="single")}
''')
def p4_rt_url(doc):
    doc.add_paragraph("L: {{rt}} R")


# 73
@fixture("p4_rt_in_table", "render: RichText inside table cell", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    return {"rt": RichText("单元格", bold=True, size="18")}
''')
def p4_rt_in_table(doc):
    t = doc.add_table(rows=2, cols=2)
    fill_row(t, 0, ["H1", "H2"])
    fill_row(t, 1, ["{{rt}}", "plain"])


# 74
@fixture("p4_rtp_basic", "render: RichTextParagraph with parastyle", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    rp = RichTextParagraph("首段", parastyle="ListBullet")
    rp.add(RichText("次段粗体", bold=True), parastyle=None)
    return {"rp": rp}
''')
def p4_rtp_basic(doc):
    doc.add_paragraph("BEFORE")
    doc.add_paragraph("{{rp}}")
    doc.add_paragraph("AFTER")


# 75
@fixture("p4_listing_basic", "render: Listing with nl/tab/bell/ff", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    return {"lst": Listing("L1\\nL2\\tT3\\aP4\\fPAGE")}
''')
def p4_listing_basic(doc):
    doc.add_paragraph("V: {{lst}} W")


# 76
@fixture("p4_listing_after_rt", "render: Listing after RichText (resolve_listing escape)",
         "P4", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {"rt": RichText("前"), "lst": Listing("X\\nY")}
''')
def p4_listing_after_rt(doc):
    doc.add_paragraph("A{{rt}}B{{lst}}C")


# 77
@fixture("p4_img_png", "render: InlineImage png native size", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    return {"img": InlineImage(tpl, _img("p4_dot2x1.png"))}
''')
def p4_img_png(doc):
    doc.add_paragraph("P:{{ img }}Q")


# 78
@fixture("p4_img_scale_w", "render: InlineImage width only (keep ratio)", "P4",
         "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {"img": InlineImage(tpl, _img("p4_wide4x1.png"), width=Mm(20))}
''')
def p4_img_scale_w(doc):
    doc.add_paragraph("W:{{ img }}")


# 79
@fixture("p4_img_wh", "render: InlineImage explicit width+height (no constraint)",
         "P4", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {"img": InlineImage(tpl, _img("p4_rect4x2.jpg"),
                               width=Mm(10), height=Mm(3))}
''')
def p4_img_wh(doc):
    doc.add_paragraph("S:{{ img }}")


# 80
@fixture("p4_img_dup", "render: same image twice (sha1 dedup, same rId)", "P4",
         "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {"i1": InlineImage(tpl, _img("p4_dot2x1.png")),
            "i2": InlineImage(tpl, _img("p4_dot2x1.png"))}
''')
def p4_img_dup(doc):
    doc.add_paragraph("A{{i1}}B{{i2}}C")


# 81
@fixture("p4_img_two", "render: two different images", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    return {"i1": InlineImage(tpl, _img("p4_dot2x1.png")),
            "i2": InlineImage(tpl, _img("p4_rect4x2.jpg"))}
''')
def p4_img_two(doc):
    doc.add_paragraph("A{{i1}}B{{i2}}C")


# 82
@fixture("p4_img_anchor", "render: InlineImage with hyperlink anchor", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    return {"img": InlineImage(tpl, _img("p4_dot2x1.png"),
                               anchor="https://example.com/")}
''')
def p4_img_anchor(doc):
    doc.add_paragraph("A:{{ img }}")


# 83
@fixture("p4_img_formats", "render: bmp/gif/tiff + template drawing (shape_id=2)",
         "P4", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {"bmp": InlineImage(tpl, _img("p4_brick4x2.bmp")),
            "gif": InlineImage(tpl, _img("p4_arrow4x2.gif")),
            "tif": InlineImage(tpl, _img("p4_tile4x2.tiff"))}
''')
def p4_img_formats(doc):
    doc.add_picture(io.BytesIO(minimal_png()), width=Inches(1))
    doc.add_paragraph("B{{bmp}}G{{gif}}T{{tif}}")


# 84
@fixture("p4_img_in_table", "render: images in table row loop", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    img = InlineImage(tpl, _img("p4_dot2x1.png"))
    return {"rows": [{"n": "r1", "img": img}, {"n": "r2", "img": img}]}
''')
def p4_img_in_table(doc):
    t = doc.add_table(rows=4, cols=2)
    fill_row(t, 0, ["Name", "Pic"])
    t.cell(1, 0).text = "{%tr for r in rows %}"
    fill_row(t, 2, ["{{r.n}}", "{{r.img}}"])
    t.cell(3, 0).text = "{%tr endfor %}"


# 85
@fixture("p4_img_bad", "render: unrecognized image error", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    return {"img": InlineImage(tpl, _img("p4_bad.png"))}
''')
def p4_img_bad(doc):
    doc.add_paragraph("E:{{ img }}")


# 86
@fixture("p4_combo_rich", "render: RichText + Listing + image + row loop combo",
         "P4", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    title = RichText("INV-2026-001", bold=True, color="1F4E79", size="24")
    rows = [
        {"name": RichText("Alpha", bold=True), "qty": 1,
         "img": InlineImage(tpl, _img("p4_dot2x1.png"))},
        {"name": RichText("Beta", italic=True), "qty": 2,
         "img": InlineImage(tpl, _img("p4_dot2x1.png"))},
    ]
    return {"title": title, "rows": rows,
            "notes": Listing("first line\\nsecond line\\tlast")}
''')
def p4_combo_rich(doc):
    doc.add_paragraph("Title: {{title}}")
    t = doc.add_table(rows=4, cols=3)
    fill_row(t, 0, ["Item", "Qty", "Pic"])
    t.cell(1, 0).text = "{%tr for r in rows %}"
    fill_row(t, 2, ["{{r.name}}", "{{r.qty}}", "{{r.img}}"])
    t.cell(3, 0).text = "{%tr endfor %}"
    doc.add_paragraph("N: {{notes}}")


# ===========================================================================
# main
# ===========================================================================

def main():
    for directory in (TEMPLATES_DIR, CONTEXTS_DIR, MEDIA_DIR):
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True, exist_ok=True)

    for name, make in sorted(MEDIA_FILES.items()):
        (MEDIA_DIR / name).write_bytes(make())

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
            if fx["context_kind"] == "python":
                context_path = CONTEXTS_DIR / (fx["id"] + ".py")
                context_path.write_text(_CTX_HEADER + fx["context_src"].lstrip("\n"),
                                        encoding="utf-8")
                context_field = "contexts/%s.py" % fx["id"]
            else:
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
            "context_kind": fx["context_kind"],
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

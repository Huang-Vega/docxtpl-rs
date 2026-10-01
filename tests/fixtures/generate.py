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
from docx.enum.style import WD_STYLE_TYPE
from docx.opc.constants import RELATIONSHIP_TYPE as RT
from docx.opc.packuri import PackURI
from docx.opc.part import Part
from docx.oxml import OxmlElement
from docx.oxml.ns import qn
from docx.shared import Inches, Pt, RGBColor

SCRIPT_DIR = Path(__file__).resolve().parent
TEMPLATES_DIR = SCRIPT_DIR / "templates"
CONTEXTS_DIR = SCRIPT_DIR / "contexts"
MEDIA_DIR = SCRIPT_DIR / "media"
# P7b: external static templates (maintained by hand / real Word, not built
# programmatically), copied into the repository verbatim.
# Source: docxtpl 0.20.2 tests/templates (LGPL-2.1-only, same license as this project).
SOURCES_DIR = SCRIPT_DIR / "sources"
MANIFEST_PATH = SCRIPT_DIR / "manifest.json"

UPSTREAM = {
    "docxtpl": "0.20.2",
    "repository": "https://github.com/elapouya/python-docx-template",
    "tag": "v0.20.2",
    "sha": "cf5437bdf5d30f9362149ddea508d6d9f008b6cd",
    "license": "LGPL-2.1-only",
}

# Fixture ids whose measured upstream outcome is "error" (all others are "ok").
EXPECTED_OVERRIDES = {
    "r2_syntax_error": "error",
    "p4_img_bad": "error",
    "p5_hf_syntax_error": "error",
    # P7: the replace_pic id never matches; pre_processing at save raises ValueError.
    "p7_pic_missing": "error",
}

W_NS = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
# P7: part CT and reltype of manually attached embedded objects (oleObject),
# verified reachable by probes.
CT_OLEOBJECT = "application/vnd.openxmlformats-officedocument.oleObject"
REL_OLEOBJECT = ("http://schemas.openxmlformats.org/officeDocument/2006/"
                 "relationships/oleObject")
CT_CUSTOMXML = "application/vnd.openxmlformats-officedocument.customXmlProperties+xml"
REL_CUSTOMXML = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml"
CT_FOOTNOTES = "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"
REL_FOOTNOTES = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes"

FIXTURES = []


def fixture(fid, feature, phase, mode, context=None, post=None,
            context_kind="json", context_src=None, sub_build=None,
            skip_render=False, autoescape=False, source=None, source_upstream=None):
    """Register a fixture.

    When source is not None this is a P7b static template: build is not
    run; sources/<source> is copied verbatim to templates/<id>.docx;
    source_upstream records the original upstream path (license attribution,
    written into the manifest's source field).
    """
    def deco(build):
        FIXTURES.append({
            "id": fid, "feature": feature, "phase": phase, "mode": mode,
            "context": context, "post": post, "build": build,
            "context_kind": context_kind, "context_src": context_src,
            "sub_build": sub_build, "skip_render": skip_render,
            "autoescape": autoescape,
            "source": source, "source_upstream": source_upstream,
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


# P5: same structure as FOOTNOTES_XML, but the body footnote carries jinja
# tags (variable + RichText + Listing); upstream treats footnotes as a
# generic binary part and writes the rendered string back verbatim.
FOOTNOTES_TAGS_XML = (
    "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n"
    '<w:footnotes xmlns:w="' + W_NS + '">'
    '<w:footnote w:type="separator" w:id="-1">'
    "<w:p><w:r><w:separator/></w:r></w:p></w:footnote>"
    '<w:footnote w:type="continuationSeparator" w:id="0">'
    "<w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>"
    "<w:footnote w:id=\"1\"><w:p><w:r><w:t>FN {{fn}} {{rt}} {{lst}}</w:t></w:r>"
    "</w:p></w:footnote>"
    "</w:footnotes>"
).encode("utf-8")


def postprocess_footnotes_tags(path):
    rewrite_zip(
        path,
        add_parts={"word/footnotes.xml": FOOTNOTES_TAGS_XML},
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
    """RGB PNG; px_per_unit is the pHYs pixels-per-metre (omit pHYs when None, dpi=72)."""
    ihdr = struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)
    raw = b"".join(b"\x00" + b"\x00\x7f\xff" * w for _ in range(h))
    out = b"\x89PNG\r\n\x1a\n" + _png_chunk(b"IHDR", ihdr)
    if px_per_unit is not None:
        out += _png_chunk(b"pHYs", struct.pack(">IIB", px_per_unit, px_per_unit, 1))
    out += _png_chunk(b"IDAT", zlib.compress(raw))
    return out + _png_chunk(b"IEND", b"")


def make_jpeg(w, h, dpi):
    """Minimal JFIF (APP0 dpi + SOF0); never decoded, only the headers matter.

    JFIF segment layout: marker(FF E0) + len + 'JFIF\\0' + ver(01 02) + units + x/y + thumbnail dims.
    units=1 means inches (python-docx then treats density as dpi; 0 degrades to 72 dpi).
    """
    jpg = b"\xff\xd8"
    jpg += (b"\xff\xe0" + struct.pack(">H", 16) + b"JFIF\x00\x01\x02\x01"
            + struct.pack(">HH", dpi, dpi) + b"\x00\x00")
    jpg += (b"\xff\xc0" + struct.pack(">H", 17) + b"\x08"
            + struct.pack(">HH", h, w) + b"\x03" + b"\x01\x11\x00" * 3)
    return jpg + b"\xff\xd9"


def make_bmp(w, h, px_per_m):
    """24bpp BITMAPINFOHEADER BMP (bottom-up, rows 4-byte aligned)."""
    row = ((w * 3 + 3) // 4) * 4
    pix = b"\x00" * (row * h)
    dib = struct.pack("<IiiHHIIiiII", 40, w, h, 1, 24, 0, len(pix),
                      px_per_m, px_per_m, 0, 0)
    off = 14 + 40
    fh = struct.pack("<2sIHHI", b"BM", off + len(pix), 0, 0, off)
    return fh + dib + pix


def make_gif(w, h):
    """GIF87a (2-color global color table; LZW data is header-only, never decoded)."""
    return (b"GIF87a" + struct.pack("<HHBBB", w, h, 0x80, 0, 0)
            + b"\x00\x00\x00\xff\xff\xff"
            + b"," + struct.pack("<HHHHB", 0, 0, w, h, 0)
            + b"\x02\x02\x2c\x01;"
            )


def make_tiff(w, h, res_num, unit=2):
    """Little-endian TIFF: IFD0 carries dimensions and RATIONAL resolutions (value area follows the IFD)."""
    res_off = 8 + 2 + 12 * 5 + 4
    entries = [
        (256, 4, 1, w),           # ImageWidth LONG
        (257, 4, 1, h),           # ImageLength LONG
        (282, 5, 1, res_off),     # XResolution RATIONAL
        (283, 5, 1, res_off),     # YResolution RATIONAL
        (296, 3, 1, unit),        # ResolutionUnit SHORT (value inline)
    ]
    ifd = b"".join(struct.pack("<HHII", t, ty, c, v) for t, ty, c, v in entries)
    return (b"II*\x00" + struct.pack("<I", 8) + struct.pack("<H", len(entries))
            + ifd + struct.pack("<I", 0) + struct.pack("<II", res_num, 1))


MEDIA_FILES = {
    # 2x1 PNG, no pHYs -> 72dpi -> EMU 25400x12700
    "p4_dot2x1.png": lambda: make_png(2, 1),
    # 4x1 PNG, pHYs 5906 px/m -> int(round(5906*0.0254))=150dpi -> 24384x6096
    "p4_wide4x1.png": lambda: make_png(4, 1, px_per_unit=5906),
    # 4x2 JFIF, 300dpi -> native EMU 12192x6096
    "p4_rect4x2.jpg": lambda: make_jpeg(4, 2, 300),
    # 4x2 BMP, 2835 px/m -> round(71.889)=72dpi
    "p4_brick4x2.bmp": lambda: make_bmp(4, 2, 2835),
    "p4_arrow4x2.gif": lambda: make_gif(4, 2),
    # 4x2 TIFF, 150/1 dpi, unit=2 (inches)
    "p4_tile4x2.tiff": lambda: make_tiff(4, 2, 150),
    # Not an image: triggers UnrecognizedImageError
    "p4_bad.png": lambda: b"this is not an image at all\n",
    # ---- P7 media-replacement family material (ADR-008) ----
    # dummy 1x1 (same as minimal_png, CRC 0x2da0f51): source picture replaced in the template.
    "p7_dummy.png": minimal_png,
    # 3x2: replacement target for body/header replace_media and replace_pic(name).
    "p7_media3x2.png": lambda: make_png(3, 2),
    # 2x2: source bytes of the second picture in p7_pic_match (different sha1, avoids image-part dedup).
    "p7_pic2x2.png": lambda: make_png(2, 2),
    # 4x4: replacement target for the title="my-title" picture.
    "p7_new4x4.png": lambda: make_png(4, 4),
    # embeddings: two orig/new pairs each (generic binary parts; a real OLE
    # format is unnecessary, python-docx saves the blob verbatim; probes
    # verified reachability).
    "p7_embed_orig1.bin": lambda: b"P7-EMBED-ONE-original-bytes-0001\n",
    "p7_embed_new1.bin": lambda: b"P7-EMBED-ONE-replaced-bytes-9999\n",
    "p7_embed_orig2.bin": lambda: b"P7-EMBED-TWO-original-bytes-0002\n",
    "p7_embed_new2.bin": lambda: b"P7-EMBED-TWO-replaced-bytes-8888\n",
}


# ===========================================================================
# P4 -- render fixtures (mode=render, typed context via build_context(tpl))
# ===========================================================================

_CTX_HEADER = '''\
# -*- coding: utf-8 -*-
"""P4 fixture context (generated by tests/fixtures/generate.py; do not edit by hand).

Provides build_context(tpl): returns the render context, which may contain
typed values such as RichText/RichTextParagraph/Listing/InlineImage/Subdoc
(matching upstream docxtpl 0.20.2 usage).
"""
import io
import os

from docx.shared import Mm
from docxtpl import InlineImage, Listing, RichText, RichTextParagraph

_HERE = os.path.dirname(os.path.abspath(__file__))


def _img(name):
    return os.path.join(_HERE, os.pardir, "media", name)


def _sub(name):
    return os.path.join(_HERE, os.pardir, "templates", name)


def _media_path(name):
    """Absolute path to material under tests/fixtures/media (P7: path arguments of replace_*)."""
    return os.path.join(_HERE, os.pardir, "media", name)


def _media_bytes(name):
    """Bytes of material under tests/fixtures/media (P7: wrapped in BytesIO for file-like arguments)."""
    with open(_media_path(name), "rb") as fh:
        return fh.read()

'''


# 70
@fixture("p4_rt_basic", "render: RichText props + concat + empty", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    rt = RichText("Chinese<b>Bold", bold=True, color="#FF0000", size="20")
    rt.add("Italic", italic=True)
    rt.add("Underline", underline=True)
    rt.add("Strikethrough", strike=True)
    rt.add("Highlight", highlight="#FFFF00")
    return {"rt": rt, "empty": RichText()}
''')
def p4_rt_basic(doc):
    doc.add_paragraph("A: {{rt}} B: {{empty}}E")


# 71
@fixture("p4_rt_style_font", "render: RichText style/font/sub/sup/lang/rtl", "P4",
         "render", context_kind="python",
         context_src='''
def build_context(tpl):
    rt1 = RichText("Style", style="Emphasis")
    rt1.add("regional font", font="eastAsia:SimSun")
    rt2 = RichText("Superscript", superscript=True, lang="zh-CN")
    rt3 = RichText("RTL", bold=True, rtl=True)
    rt3.add("Subscript", subscript=True)
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
    return {"rt": RichText("Link text", url_id=url_id, color="0563C1",
                           underline="single")}
''')
def p4_rt_url(doc):
    doc.add_paragraph("L: {{rt}} R")


# 73
@fixture("p4_rt_in_table", "render: RichText inside table cell", "P4", "render",
         context_kind="python",
         context_src='''
def build_context(tpl):
    return {"rt": RichText("Cell", bold=True, size="18")}
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
    rp = RichTextParagraph("First paragraph", parastyle="ListBullet")
    rp.add(RichText("Second paragraph bold", bold=True), parastyle=None)
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
    return {"rt": RichText("Before"), "lst": Listing("X\\nY")}
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


@fixture("p4_autoescape_rich",
         "render: autoescape=True with RichText, Listing and InlineImage",
         "P4", "render", context_kind="python", autoescape=True,
         context_src='''
def build_context(tpl):
    return {"rt": RichText("R<&", bold=True),
            "lst": Listing("L<&\\nN"),
            "img": InlineImage(tpl, _img("p4_dot2x1.png"))}
''')
def p4_autoescape_rich(doc):
    doc.add_paragraph("A{{rt}}B{{lst}}C{{img}}D")


# ===========================================================================
# P5 -- headers / footers / footnotes multi-part rendering (ADR-006)
# ===========================================================================

def _unlink_part(part):
    """Unlink a header/footer from "link to previous" so it gets its own part (python-docx)."""
    part.is_linked_to_previous = False
    return part


# 87
@fixture("p5_hf_basic", "render: header vars/if/for + footer var + body", "P5",
         "render", context={"htitle": "Header title", "hshow": True,
                             "hitems": ["A", "B", "C"],
                             "ftitle": "Footer title", "pageno": 7,
                             "btitle": "Body title"})
def p5_hf_basic(doc):
    sec = doc.sections[0]
    header = _unlink_part(sec.header)
    header.paragraphs[0].text = "HDR {{htitle}}"
    header.add_paragraph("{% if hshow %}VISIBLE{% endif %}")
    header.add_paragraph("{%p for it in hitems %}")
    header.add_paragraph("ITEM {{it}}")
    header.add_paragraph("{%p endfor %}")
    footer = _unlink_part(sec.footer)
    footer.paragraphs[0].text = "FTR {{ftitle}} / P{{pageno}}"
    doc.add_paragraph("BODY {{btitle}}")


# 88
@fixture("p5_hf_multi", "render: two sections with independent headers/footers",
         "P5", "render", context={"h1": "Header one", "f1": "Footer one", "b1": "Section one body",
                                   "h2": "Header two", "f2": "Footer two", "b2": "Section two body"})
def p5_hf_multi(doc):
    sec0 = doc.sections[0]
    _unlink_part(sec0.header).paragraphs[0].text = "H1 {{h1}}"
    _unlink_part(sec0.footer).paragraphs[0].text = "F1 {{f1}}"
    doc.add_paragraph("SEC1 {{b1}}")

    doc.add_section(WD_SECTION.NEW_PAGE)
    sec1 = doc.sections[1]
    _unlink_part(sec1.header).paragraphs[0].text = "H2 {{h2}}"
    _unlink_part(sec1.footer).paragraphs[0].text = "F2 {{f2}}"
    doc.add_paragraph("SEC2 {{b2}}")


# 89
@fixture("p5_footnotes_basic", "render: footnotes part vars + RichText + Listing",
         "P5", "render", context_kind="python", post=postprocess_footnotes_tags,
         context_src='''
def build_context(tpl):
    return {"bn": "Body reference", "fn": "Footnote value",
            "rt": RichText("Rich text", bold=True),
            "lst": Listing("L1\\nL2\\tT2")}
''')
def p5_footnotes_basic(doc):
    doc.add_paragraph("MAIN {{bn}}")


# 90
@fixture("p5_hf_image",
         "render: same image in body/header/footer (package dedup, 3 rels scopes)",
         "P5", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {
        "ht": "Header",
        "himg": InlineImage(tpl, _img("p4_dot2x1.png")),
        "himg2": InlineImage(tpl, _img("p4_dot2x1.png"),
                             anchor="https://example.com/anchor"),
        "ft": "Footer",
        "fimg": InlineImage(tpl, _img("p4_dot2x1.png")),
        "bt": "Body",
        "bimg": InlineImage(tpl, _img("p4_dot2x1.png")),
    }
''')
def p5_hf_image(doc):
    sec = doc.sections[0]
    _unlink_part(sec.header).paragraphs[0].text = "HDR {{ht}} {{himg}} {{himg2}}"
    _unlink_part(sec.footer).paragraphs[0].text = "FTR {{ft}} {{fimg}}"
    doc.add_paragraph("BODY {{bt}} {{bimg}}")


# 91
@fixture("p5_hf_richtext", "render: RichText/Listing in header, RichText in footer",
         "P5", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    url_id = tpl.build_url_id("https://docxtpl.readthedocs.io/p5")
    return {"hrt": RichText("Header rich", bold=True, color="1F4E79"),
            "hlst": Listing("HL1\\nHL2"),
            "frt": RichText("Footer rich", italic=True),
            "brt": RichText("Body link", url_id=url_id,
                            underline="single", color="0563C1")}
''')
def p5_hf_richtext(doc):
    sec = doc.sections[0]
    _unlink_part(sec.header).paragraphs[0].text = "H {{hrt}} {{hlst}}"
    _unlink_part(sec.footer).paragraphs[0].text = "F {{frt}}"
    doc.add_paragraph("B {{brt}}")


# 92
@fixture("p5_hf_syntax_error", "render: jinja syntax error in header part", "P5",
         "render", context={"x": True})
def p5_hf_syntax_error(doc):
    sec = doc.sections[0]
    _unlink_part(sec.header).paragraphs[0].text = "HDR {% if x %}missing endif"
    doc.add_paragraph("BODY {{x}}")


# 93
@fixture("p5_hf_untagged", "render: untagged header/footer roundtrip (lxml reserialize)",
         "P5", "render", context={"unused": "unused"})
def p5_hf_untagged(doc):
    sec = doc.sections[0]
    _unlink_part(sec.header).paragraphs[0].text = "Plain Header untagged"
    _unlink_part(sec.footer).paragraphs[0].text = "Plain Footer untagged"
    doc.add_paragraph("Plain body without tags")


# ===========================================================================
# P6 -- subdoc (external docx merged via tpl.new_subdoc, ADR-007)
#
# Main template is uniform across fixtures: a plain text paragraph plus a
# paragraph owned solely by `{{p sd }}` (expression form, not {%p %}).
# The main template avoids bookmarks/pictures/headers/footers/multiple sections
# everywhere (so the renumber trio and fix_section_types are no-ops on the
# main document); the sub docx avoids w:numId/footnote references/
# SmartArt/VML/custom properties/multiple sections (upstream semantics there
# are nondeterministic or unsupported by the main package).
# ===========================================================================

def _sub_doc_basic(doc):
    doc.add_paragraph("Sub first paragraph")
    doc.add_paragraph("Sub second paragraph")
    doc.add_paragraph("Sub third paragraph")


# 94
@fixture("p6_subdoc_basic", "render: subdoc with plain paragraphs (minimal attach)",
         "P6", "render", context_kind="python", sub_build=_sub_doc_basic,
         context_src='''
def build_context(tpl):
    sd = tpl.new_subdoc(_sub("p6_subdoc_basic_sub.docx"))
    return {"sd": sd}
''')
def p6_subdoc_basic(doc):
    doc.add_paragraph("Main text before subdoc")
    doc.add_paragraph("{{p sd }}")


def _sub_doc_style(doc):
    doc.add_paragraph("Plain default paragraph")
    doc.add_paragraph("Sub Heading", style="Heading 1")
    custom = doc.styles.add_style("MyCustom", WD_STYLE_TYPE.PARAGRAPH)
    doc.add_paragraph("Custom style paragraph", style=custom)


# 95
@fixture("p6_subdoc_style", "render: subdoc styles (mapped Heading 1 + appended custom)",
         "P6", "render", context_kind="python", sub_build=_sub_doc_style,
         context_src='''
def build_context(tpl):
    sd = tpl.new_subdoc(_sub("p6_subdoc_style_sub.docx"))
    return {"sd": sd}
''')
def p6_subdoc_style(doc):
    doc.add_paragraph("Main text before subdoc")
    doc.add_paragraph("{{p sd }}")


def _sub_doc_image(doc):
    doc.add_picture(io.BytesIO(minimal_png()), width=Inches(1))
    p = doc.add_paragraph()
    r_id = doc.part.relate_to("https://example.com/sub-link", RT.HYPERLINK,
                              is_external=True)
    hyperlink = OxmlElement("w:hyperlink")
    hyperlink.set(qn("r:id"), r_id)
    run = OxmlElement("w:r")
    t = OxmlElement("w:t")
    t.text = "Sub external link"
    run.append(t)
    hyperlink.append(run)
    p._p.append(hyperlink)


# 96
@fixture("p6_subdoc_image",
         "render: subdoc image (sha1 dedup, media part, r:embed rewrite) + external link",
         "P6", "render", context_kind="python", sub_build=_sub_doc_image,
         context_src='''
def build_context(tpl):
    sd = tpl.new_subdoc(_sub("p6_subdoc_image_sub.docx"))
    return {"sd": sd}
''')
def p6_subdoc_image(doc):
    doc.add_paragraph("Main text before subdoc")
    doc.add_paragraph("{{p sd }}")


def _sub_doc_verbatim(doc):
    doc.add_paragraph("literal {% x %} stays")
    doc.add_paragraph("literal {{ y }} stays too")


# 97
@fixture("p6_subdoc_verbatim", "render: subdoc with verbatim jinja tag text (single pass)",
         "P6", "render", context_kind="python", sub_build=_sub_doc_verbatim,
         context_src='''
def build_context(tpl):
    sd = tpl.new_subdoc(_sub("p6_subdoc_verbatim_sub.docx"))
    return {"sd": sd}
''')
def p6_subdoc_verbatim(doc):
    doc.add_paragraph("Main text before subdoc")
    doc.add_paragraph("{{p sd }}")


def _sub_doc_untagged(doc):
    pass  # empty body (sectPr only): the fragment is an empty string


# 98
@fixture("p6_subdoc_untagged", "render: subdoc without any tags (attach tree transform only)",
         "P6", "render", context_kind="python", sub_build=_sub_doc_untagged,
         context_src='''
def build_context(tpl):
    sd = tpl.new_subdoc(_sub("p6_subdoc_untagged_sub.docx"))
    return {"sd": sd}
''')
def p6_subdoc_untagged(doc):
    doc.add_paragraph("{{p sd }}")


# ===========================================================================
# P7 -- media/embedded replacement family + variable introspection (ADR-008)
#
# Path B (post_processing, zip layer): replace_media / replace_embedded /
# replace_zipname swap only the blob; partname/CT/rels/wp:extent all stay put.
# Path A (pre_processing, part blob after render): replace_pic matches by cNvPr
# name/title/descr; a miss raises ValueError. skip_render pins "save without
# render" (docPr ids are not renumbered).
# ===========================================================================

# 99
@fixture("p7_media_body", "render: replace_media by CRC (body picture, partname/CT/extent unchanged)",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    tpl.replace_media(io.BytesIO(_media_bytes("p7_dummy.png")),
                      io.BytesIO(_media_bytes("p7_media3x2.png")))
    return {"x": "body media replacement"}
''')
def p7_media_body(doc):
    doc.add_paragraph("media replacement body {{ x }}")
    doc.add_picture(io.BytesIO(minimal_png()), width=Inches(1))


# 100
@fixture("p7_media_header", "render: replace_media by CRC hits header dummy picture",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    tpl.replace_media(io.BytesIO(_media_bytes("p7_dummy.png")),
                      io.BytesIO(_media_bytes("p7_media3x2.png")))
    return {"y": "header media replacement"}
''')
def p7_media_header(doc):
    # Header placeholder dummy picture (the signature replace_media case:
    # template pictures cannot be added to a header).
    sec = doc.sections[0]
    sec.header.paragraphs[0].add_run().add_picture(
        io.BytesIO(minimal_png()), width=Inches(0.5))
    doc.add_paragraph("header picture replacement body {{ y }}")


# 101
@fixture("p7_pic_match", "render: replace_pic by cNvPr name and title (two body pictures)",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    tpl.replace_pic("image.png", io.BytesIO(_media_bytes("p7_media3x2.png")))
    tpl.replace_pic("my-title", io.BytesIO(_media_bytes("p7_new4x4.png")))
    return {"z": "picture marker replacement"}
''')
def p7_pic_match(doc):
    doc.add_paragraph("picture marker replacement {{ z }}")
    # The two pictures have distinct bytes -> independent media parts. The
    # first keeps python-docx's default name "image.png"; the second is
    # renamed by hand and given a title (use a name that takes no part in
    # registration; otherwise, with upstream dict insertion order and
    # break-on-first-hit, the image.png key would shadow the title key first,
    # so my-title could never match).
    doc.add_picture(io.BytesIO(minimal_png()), width=Inches(1))
    doc.add_picture(io.BytesIO(make_png(2, 2)), width=Inches(0.8))
    cnvprs = doc.element.body.xpath("//pic:cNvPr")
    cnvprs[1].set("name", "titled-2x2.png")
    cnvprs[1].set("title", "my-title")


# 102 (expected error: ValueError)
@fixture("p7_pic_missing", "render: replace_pic id never matched -> ValueError at save",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    tpl.replace_pic("nope.png", io.BytesIO(_media_bytes("p7_media3x2.png")))
    return {"w": "missing marker"}
''')
def p7_pic_missing(doc):
    doc.add_paragraph("missing marker {{ w }}")
    doc.add_picture(io.BytesIO(minimal_png()), width=Inches(1))


# 103
@fixture("p7_embedded_zipname",
         "render: replace_embedded (CRC) + replace_zipname (exact) on manual oleObject parts",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    # Upstream replace_embedded accepts only a file path for dst; src may be
    # file-like.
    tpl.replace_embedded(io.BytesIO(_media_bytes("p7_embed_orig1.bin")),
                         _media_path("p7_embed_new1.bin"))
    tpl.replace_zipname("word/embeddings/p7_ole2.bin",
                        _media_path("p7_embed_new2.bin"))
    return {"e": "embedded replacement"}
''')
def p7_embedded_zipname(doc):
    doc.add_paragraph("embedded object replacement {{ e }}")
    package = doc.part.package
    # Probe-verified: an embeddings part attached via Part + relate_to is
    # saved by python-docx (reachable via the graph walk; the CT Override is
    # kept in sync).
    part1 = Part(PackURI("/word/embeddings/p7_ole1.bin"),
                 CT_OLEOBJECT, _p7_media("p7_embed_orig1.bin"), package)
    doc.part.relate_to(part1, REL_OLEOBJECT)
    part2 = Part(PackURI("/word/embeddings/p7_ole2.bin"),
                 CT_OLEOBJECT, _p7_media("p7_embed_orig2.bin"), package)
    doc.part.relate_to(part2, REL_OLEOBJECT)


def _p7_media(name):
    """Bytes from the same source as the generated material (used when
    attaching Parts at build time; reads the already-written file under
    media/)."""
    return (Path(__file__).resolve().parent / "media" / name).read_bytes()


# 104 (skip_render: save without render; docPr keeps the template's original id=1)
@fixture("p7_replace_only", "save without render: replace_media only (no docPr renumber)",
         "P7", "render", context_kind="python", skip_render=True,
         context_src='''
def build_context(tpl):
    tpl.replace_media(io.BytesIO(_media_bytes("p7_dummy.png")),
                      io.BytesIO(_media_bytes("p7_media3x2.png")))
    return {}
''')
def p7_replace_only(doc):
    doc.add_paragraph("plain replacement static paragraph (no jinja tags)")
    doc.add_picture(io.BytesIO(minimal_png()), width=Inches(1))


# 105
@fixture("p7_undeclared_vars",
         "render: get_undeclared_template_variables feeds the variable list into ctx",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    names = sorted(tpl.get_undeclared_template_variables())
    return {"vars": ",".join(names)}
''')
def p7_undeclared_vars(doc):
    doc.add_paragraph("{{ a }}")
    doc.add_paragraph("{% if b %}has B {{ c }}{% endif %}")
    # The p tag must own its paragraph (writing for/endfor in the same
    # paragraph makes jinja report an unknown tag).
    doc.add_paragraph("{%p for item in items %}")
    doc.add_paragraph("{{ item.name }}")
    doc.add_paragraph("{%p endfor %}")
    doc.add_paragraph("variable list: {{ vars }}")


# ===========================================================================
# P7b -- real-Word hand-built template corpus (static external templates;
# source = docxtpl 0.20.2 tests/templates, LGPL-2.1-only; not built
# programmatically -- only the context and source attribution are registered)
# ===========================================================================

# 106
@fixture("p7b_order", "render: real-Word invoice (if/for/nested tables, raw rsids)",
         "P7", "render",
         context={
             "customer_name": "Eric",
             "items": [
                 {"desc": "Python interpreters", "qty": 2, "price": "FREE"},
                 {"desc": "Django projects", "qty": 5403, "price": "FREE"},
                 {"desc": "Guido", "qty": 1, "price": "100,000,000.00"},
             ],
             "in_europe": True,
             "is_paid": False,
             "company_name": "The World Wide company",
             "total_price": "100,000,000.00",
         },
         source="p7b_order.docx", source_upstream="tests/templates/order_tpl.docx")
def _p7b_order(doc):
    pass


# 107
@fixture("p7b_dynamic_table",
         "render: real-Word dynamic table (|count filter, nested for)",
         "P7", "render",
         context={
             "col_labels": ["fruit", "vegetable", "stone", "thing"],
             "tbl_contents": [
                 {"label": "yellow", "cols": ["banana", "capsicum", "pyrite", "taxi"]},
                 {"label": "red", "cols": ["apple", "tomato", "cinnabar", "doubledecker"]},
                 {"label": "green", "cols": ["guava", "cucumber", "aventurine", "card"]},
             ],
         },
         source="p7b_dynamic_table.docx",
         source_upstream="tests/templates/dynamic_table_tpl.docx")
def _p7b_dynamic_table(doc):
    pass


# 108
@fixture("p7b_merge_paragraph",
         "render: real-Word run splits, literal {_%- text, {%- whitespace control",
         "P7", "render", context={"living_in_town": True},
         source="p7b_merge_paragraph.docx",
         source_upstream="tests/templates/merge_paragraph_tpl.docx")
def _p7b_merge_paragraph(doc):
    pass


# 109 (python: two RichText space/tab values)
@fixture("p7b_word2016",
         "render: real-Word space/tab preservation with literal string expressions",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {
        "test_space": "          ",
        "test_tabs": 5 * "\\t",
        "test_space_r": RichText("          "),
        "test_tabs_r": RichText(5 * "\\t"),
    }
''',
         source="p7b_word2016.docx",
         source_upstream="tests/templates/word2016_tpl.docx")
def _p7b_word2016(doc):
    pass


# 110
@fixture("p7b_hf_entities",
         "render: header/footer entities inside jinja expressions (default filter)",
         "P7", "render", context={"title": "Header and footer test"},
         source="p7b_hf_entities.docx",
         source_upstream="tests/templates/header_footer_entities_tpl.docx")
def _p7b_hf_entities(doc):
    pass


# 111
@fixture("p7b_comments",
         "render: real-Word {#tr/tc#} comment removal; comments part passthrough",
         "P7", "render", context={},
         source="p7b_comments.docx",
         source_upstream="tests/templates/comments_tpl.docx")
def _p7b_comments(doc):
    pass


# 112
@fixture("p7b_nested_for", "render: real-Word nested for loops with customXml parts",
         "P7", "render",
         context={
             "dishes": [
                 {"name": "Pizza",
                  "ingredients": ["bread", "tomato", "ham", "cheese"]},
                 {"name": "Hamburger",
                  "ingredients": ["bread", "chopped steak", "cheese", "sauce"]},
                 {"name": "Apple pie",
                  "ingredients": ["flour", "apples", "suggar", "quince jelly"]},
             ],
             "authors": [
                 {"name": "Saint-Exupery", "books": [
                     {"title": "Le petit prince"},
                     {"title": "L'aviateur"},
                     {"title": "Vol de nuit"}]},
                 {"name": "Barjavel", "books": [
                     {"title": "Ravage"},
                     {"title": "La nuit des temps"},
                     {"title": "Le grand secret"}]},
             ],
         },
         source="p7b_nested_for.docx",
         source_upstream="tests/templates/nested_for_tpl.docx")
def _p7b_nested_for(doc):
    pass


# 113
@fixture("p7b_preserve_spaces",
         "render: spaces around tags must not be lost in real-Word runs",
         "P7", "render", context={"tag_1": "looking", "tag_2": "too"},
         source="p7b_preserve_spaces.docx",
         source_upstream="tests/templates/preserve_spaces_tpl.docx")
def _p7b_preserve_spaces(doc):
    pass


# 114
@fixture("p7b_vm", "render: real-Word vertical merge with literal tag teaching text",
         "P7", "render",
         context={
             "items": [
                 {"desc": "Python interpreters", "qty": 2, "price": "FREE"},
                 {"desc": "Django projects", "qty": 5403, "price": "FREE"},
                 {"desc": "Guido", "qty": 1, "price": "100,000,000.00"},
             ],
             "total_price": "100,000,000.00",
             "category": "Book",
         },
         source="p7b_vm.docx",
         source_upstream="tests/templates/vertical_merge_tpl.docx")
def _p7b_vm(doc):
    pass


# 115
@fixture("p7b_vm_nested",
         "render: real-Word nested vertical merge with redundant local xmlns",
         "P7", "render", context={},
         source="p7b_vm_nested.docx",
         source_upstream="tests/templates/vertical_merge_nested_tpl.docx")
def _p7b_vm_nested(doc):
    pass


# 116
@fixture("p7b_hm", "render: real-Word horizontal merge with inline literal lists",
         "P7", "render", context={},
         source="p7b_hm.docx",
         source_upstream="tests/templates/horizontal_merge_tpl.docx")
def _p7b_hm(doc):
    pass


# 117 (python: four RichText desc values)
@fixture("p7b_cellbg", "render: real-Word cellbg rows with RichText cells",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {"alerts": [
        {"date": "2015-03-10",
         "desc": RichText("Very critical alert", color="FF0000", bold=True),
         "type": "CRITICAL", "bg": "FF0000"},
        {"date": "2015-03-11", "desc": RichText("Just a warning"),
         "type": "WARNING", "bg": "FFDD00"},
        {"date": "2015-03-12", "desc": RichText("Information"),
         "type": "INFO", "bg": "8888FF"},
        {"date": "2015-03-13", "desc": RichText("Debug trace"),
         "type": "DEBUG", "bg": "FF00FF"},
    ]}
''',
         source="p7b_cellbg.docx",
         source_upstream="tests/templates/cellbg_tpl.docx")
def _p7b_cellbg(doc):
    pass


# 118 (python: RichText + {%p if%})
@fixture("p7b_richtext_if",
         "render: RichText value inside real-Word {%p if%} paragraph",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {"foobar": RichText("Foobar!", color="ff0000")}
''',
         source="p7b_richtext_if.docx",
         source_upstream="tests/templates/richtext_and_if_tpl.docx")
def _p7b_richtext_if(doc):
    pass


# 119 (python: eastAsia font prefix)
@fixture("p7b_eastasia", "render: RichText eastAsia: font prefix (rFonts w:eastAsia)",
         "P7", "render", context_kind="python",
         context_src='''
def build_context(tpl):
    return {
        "example": RichText("TestTEST", font="eastAsia:Microsoft YaHei"),
        "Chinese": RichText("TestTEST", font="eastAsia:Microsoft YaHei"),
        "simsun": RichText("TestTEST", font="eastAsia:SimSun"),
    }
''',
         source="p7b_eastasia.docx",
         source_upstream="tests/templates/richtext_eastAsia_tpl.docx")
def _p7b_eastasia(doc):
    pass


# 120
@fixture("p7b_less_cells",
         "render: fix_tables cell removal with entities inside inline literal list",
         "P7", "render", context={},
         source="p7b_less_cells.docx",
         source_upstream="tests/templates/less_cells_after_loop_tpl.docx")
def _p7b_less_cells(doc):
    pass


# 121
@fixture("p7b_footnotes_real",
         "render: real-Word footnotes part round-trip through jinja",
         "P7", "render", context={"a_jinja_variable": "A Jinja variable!"},
         source="p7b_footnotes_real.docx",
         source_upstream="tests/templates/footnotes_tpl.docx")
def _p7b_footnotes_real(doc):
    pass


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
        template_path = TEMPLATES_DIR / (fx["id"] + ".docx")
        if fx["source"] is not None:
            # P7b static external template: copied verbatim, not built
            # programmatically.
            src = SOURCES_DIR / fx["source"]
            if not src.exists():
                raise FileNotFoundError("missing static template source file: %s" % src)
            shutil.copyfile(src, template_path)
        else:
            doc = Document()
            fx["build"](doc)
            doc.save(template_path)
        if fx["post"] is not None:
            fx["post"](template_path)
        if fx["sub_build"] is not None:
            # P6 sub docx: the external document to be merged in (generated by
            # python-docx; an input, not an output).
            sub_doc = Document()
            fx["sub_build"](sub_doc)
            sub_doc.save(TEMPLATES_DIR / (fx["id"] + "_sub.docx"))

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

        record = {
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
            "owner": "docxtpl-rs",
            "issue": "",
        }
        # P7: emit the field only when True (both runner/oracle sides treat
        # absence as false).
        if fx["skip_render"]:
            record["skip_render"] = True
        if fx["autoescape"]:
            record["autoescape"] = True
        # P7b: license/source attribution for the static external template.
        if fx["source"] is not None:
            record["source"] = "docxtpl-0.20.2:%s" % fx["source_upstream"]
        records.append(record)

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

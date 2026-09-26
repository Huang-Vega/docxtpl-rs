"""Python-oracle probe for the high-risk ``docxcompose`` Subdoc paths.

The probe starts from the small P6 fixtures, injects the minimum package
parts needed by each feature, composes them with the pinned Python
``docxtpl==0.20.2`` / ``docxcompose==2.2.0`` stack, and reports semantic facts
as JSON.  All generated files stay below ``target/`` (or ``--work-dir``).

Covered paths:

* VML ``v:imagedata/@r:id`` image copying;
* SmartArt dm/lo/qs/cs part copying and duplicate-reference reuse;
* standard package-root ``docProps/custom.xml`` fields (simple and complex);
* footnote id and relationship copying, both with and without a main part.
"""

from __future__ import annotations

import argparse
import base64
import importlib.metadata
import json
import posixpath
import sys
import zipfile
from pathlib import Path
from typing import Callable, Iterable

import docxtpl
from docxtpl import DocxTemplate
from lxml import etree


ROOT = Path(__file__).resolve().parents[2]
MAIN_FIXTURE = ROOT / "tests/fixtures/templates/p6_subdoc_basic.docx"
SUB_FIXTURE = ROOT / "tests/fixtures/templates/p6_subdoc_basic_sub.docx"

NS = {
    "a": "http://schemas.openxmlformats.org/drawingml/2006/main",
    "cp": "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties",
    "ct": "http://schemas.openxmlformats.org/package/2006/content-types",
    "dgm": "http://schemas.openxmlformats.org/drawingml/2006/diagram",
    "pr": "http://schemas.openxmlformats.org/package/2006/relationships",
    "r": "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    "v": "urn:schemas-microsoft-com:vml",
    "vt": "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes",
    "w": "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
}

RT_IMAGE = NS["r"] + "/image"
RT_HYPERLINK = NS["r"] + "/hyperlink"
RT_FOOTNOTES = NS["r"] + "/footnotes"
RT_CUSTOM_PROPERTIES = NS["r"] + "/custom-properties"
RT_DIAGRAM = {
    "dm": NS["r"] + "/diagramData",
    "lo": NS["r"] + "/diagramLayout",
    "qs": NS["r"] + "/diagramQuickStyle",
    "cs": NS["r"] + "/diagramColors",
}

CT_FOOTNOTES = (
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml"
)
CT_CUSTOM_PROPERTIES = (
    "application/vnd.openxmlformats-officedocument.custom-properties+xml"
)
CT_DIAGRAM = {
    "dm": "application/vnd.openxmlformats-officedocument.drawingml.diagramData+xml",
    "lo": "application/vnd.openxmlformats-officedocument.drawingml.diagramLayout+xml",
    "qs": "application/vnd.openxmlformats-officedocument.drawingml.diagramStyle+xml",
    "cs": "application/vnd.openxmlformats-officedocument.drawingml.diagramColors+xml",
}

PNG_1X1 = base64.b64decode(
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8A"
    "AQUBAScY42YAAAAASUVORK5CYII="
)


def _xml(data: bytes) -> etree._Element:
    return etree.fromstring(data)


def _serialized(root: etree._Element) -> bytes:
    return etree.tostring(
        root, xml_declaration=True, encoding="UTF-8", standalone=True
    )


def rewrite_docx(
    source: Path,
    destination: Path,
    transform: Callable[[str, bytes], bytes | None],
    additions: dict[str, bytes] | None = None,
) -> None:
    """Rewrite a package once, optionally adding previously absent parts."""

    additions = dict(additions or {})
    destination.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(source) as src, zipfile.ZipFile(
        destination, "w", compression=zipfile.ZIP_DEFLATED
    ) as dst:
        seen: set[str] = set()
        for info in src.infolist():
            data = transform(info.filename, src.read(info.filename))
            seen.add(info.filename)
            if data is not None:
                dst.writestr(info, data)
        overlap = seen.intersection(additions)
        if overlap:
            raise AssertionError(f"attempted to add existing ZIP members: {sorted(overlap)}")
        for name, data in additions.items():
            dst.writestr(name, data)


def _inject_before_final_sect_pr(document: bytes, fragment: str) -> bytes:
    text = document.decode("utf-8")
    at = text.rfind("<w:sectPr")
    if at < 0:
        raise AssertionError("fixture has no final w:sectPr")
    return (text[:at] + fragment + text[at:]).encode("utf-8")


def _add_relationship(
    rels: bytes,
    *,
    rid: str,
    rel_type: str,
    target: str,
    external: bool = False,
) -> bytes:
    root = _xml(rels)
    rel = etree.SubElement(root, f"{{{NS['pr']}}}Relationship")
    rel.set("Id", rid)
    rel.set("Type", rel_type)
    rel.set("Target", target)
    if external:
        rel.set("TargetMode", "External")
    return _serialized(root)


def _add_overrides(content_types: bytes, values: Iterable[tuple[str, str]]) -> bytes:
    root = _xml(content_types)
    for part_name, content_type in values:
        node = etree.SubElement(root, f"{{{NS['ct']}}}Override")
        node.set("PartName", part_name)
        node.set("ContentType", content_type)
    return _serialized(root)


def _ensure_png_default(content_types: bytes) -> bytes:
    root = _xml(content_types)
    matches = [
        node
        for node in root.xpath("ct:Default", namespaces=NS)
        if (node.get("Extension") or "").lower() == "png"
    ]
    if not matches:
        node = etree.SubElement(root, f"{{{NS['ct']}}}Default")
        node.set("Extension", "png")
        node.set("ContentType", "image/png")
    return _serialized(root)


def _read_part(docx: Path, name: str) -> bytes:
    with zipfile.ZipFile(docx) as archive:
        return archive.read(name)


def _part_names(docx: Path) -> set[str]:
    with zipfile.ZipFile(docx) as archive:
        return set(archive.namelist())


def _xml_part(docx: Path, name: str) -> etree._Element:
    return _xml(_read_part(docx, name))


def _relationship_map(docx: Path, rels_name: str) -> dict[str, etree._Element]:
    root = _xml_part(docx, rels_name)
    return {node.get("Id"): node for node in root.xpath("pr:Relationship", namespaces=NS)}


def _resolved_target(source_part: str, target: str) -> str:
    return posixpath.normpath(posixpath.join(posixpath.dirname(source_part), target))


def compose(main: Path, sub: Path, output: Path) -> str:
    template = DocxTemplate(str(main))
    subdoc = template.new_subdoc(str(sub))
    fragment = str(subdoc)
    template.render({"sd": subdoc})
    template.save(str(output))
    return fragment


def make_vml_subdoc(destination: Path) -> None:
    fragment = (
        '<w:p><w:r><w:pict><v:shape xmlns:v="urn:schemas-microsoft-com:vml" '
        'id="vml-oracle"><v:imagedata '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" '
        'r:id="rIdVML"/></v:shape></w:pict></w:r></w:p>'
    )

    def transform(name: str, data: bytes) -> bytes:
        if name == "word/document.xml":
            return _inject_before_final_sect_pr(data, fragment)
        if name == "word/_rels/document.xml.rels":
            return _add_relationship(
                data,
                rid="rIdVML",
                rel_type=RT_IMAGE,
                target="media/vml-oracle.png",
            )
        if name == "[Content_Types].xml":
            return _ensure_png_default(data)
        return data

    rewrite_docx(
        SUB_FIXTURE,
        destination,
        transform,
        {"word/media/vml-oracle.png": PNG_1X1},
    )


def probe_vml(work: Path) -> dict[str, object]:
    sub = work / "vml-sub.docx"
    output = work / "vml-output.docx"
    make_vml_subdoc(sub)
    compose(MAIN_FIXTURE, sub, output)
    document = _xml_part(output, "word/document.xml")
    ids = document.xpath(
        './/v:shape[@id="vml-oracle"]/v:imagedata/@r:id', namespaces=NS
    )
    rels = _relationship_map(output, "word/_rels/document.xml.rels")
    rel = rels[ids[0]] if len(ids) == 1 else None
    target = _resolved_target("word/document.xml", rel.get("Target")) if rel is not None else ""
    return {
        "imagedata_ids": ids,
        "relationship_type": rel.get("Type") if rel is not None else None,
        "target": target,
        "target_exists": target in _part_names(output),
        "image_bytes_match": bool(target) and _read_part(output, target) == PNG_1X1,
    }


def make_smartart_subdoc(destination: Path) -> None:
    rel_ids = (
        '<dgm:relIds xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships" '
        'r:dm="rIdDgmData" r:lo="rIdDgmLayout" '
        'r:qs="rIdDgmStyle" r:cs="rIdDgmColors"/>'
    )
    fragment = f'<w:p><w:r><w:drawing>{rel_ids}{rel_ids}</w:drawing></w:r></w:p>'
    rid_and_kind = [
        ("rIdDgmData", "dm"),
        ("rIdDgmLayout", "lo"),
        ("rIdDgmStyle", "qs"),
        ("rIdDgmColors", "cs"),
    ]
    parts = {
        "word/diagrams/data1.xml": b'<dgm:dataModel xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram"/>',
        "word/diagrams/layout1.xml": b'<dgm:layoutDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" uniqueId="oracle-layout"/>',
        "word/diagrams/quickStyle1.xml": b'<dgm:styleDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" uniqueId="oracle-style"/>',
        "word/diagrams/colors1.xml": b'<dgm:colorsDef xmlns:dgm="http://schemas.openxmlformats.org/drawingml/2006/diagram" uniqueId="oracle-colors"/>',
    }
    part_for_kind = {
        "dm": "word/diagrams/data1.xml",
        "lo": "word/diagrams/layout1.xml",
        "qs": "word/diagrams/quickStyle1.xml",
        "cs": "word/diagrams/colors1.xml",
    }

    def transform(name: str, data: bytes) -> bytes:
        if name == "word/document.xml":
            return _inject_before_final_sect_pr(data, fragment)
        if name == "word/_rels/document.xml.rels":
            for rid, kind in rid_and_kind:
                data = _add_relationship(
                    data,
                    rid=rid,
                    rel_type=RT_DIAGRAM[kind],
                    target=posixpath.relpath(part_for_kind[kind], "word"),
                )
            return data
        if name == "[Content_Types].xml":
            return _add_overrides(
                data,
                (("/" + part_for_kind[kind], CT_DIAGRAM[kind]) for kind in RT_DIAGRAM),
            )
        return data

    rewrite_docx(SUB_FIXTURE, destination, transform, parts)


def probe_smartart(work: Path) -> dict[str, object]:
    sub = work / "smartart-sub.docx"
    output = work / "smartart-output.docx"
    make_smartart_subdoc(sub)
    compose(MAIN_FIXTURE, sub, output)
    document = _xml_part(output, "word/document.xml")
    nodes = document.xpath(".//dgm:relIds", namespaces=NS)
    rels = _relationship_map(output, "word/_rels/document.xml.rels")
    facts: dict[str, object] = {"rel_ids_count": len(nodes)}
    targets: list[str] = []
    for kind in RT_DIAGRAM:
        attr = f"{{{NS['r']}}}{kind}"
        rids = [node.get(attr) for node in nodes]
        selected = [rels[rid] for rid in rids if rid in rels]
        resolved = [
            _resolved_target("word/document.xml", rel.get("Target")) for rel in selected
        ]
        facts[kind] = {
            "same_rid_reused": len(rids) == 2 and len(set(rids)) == 1,
            "relationship_type_ok": len(selected) == 2
            and all(rel.get("Type") == RT_DIAGRAM[kind] for rel in selected),
            "target_exists": len(resolved) == 2
            and all(target in _part_names(output) for target in resolved),
        }
        targets.extend(resolved)
    facts["four_distinct_targets"] = len(set(targets)) == 4
    return facts


def make_custom_properties_subdoc(destination: Path) -> None:
    simple = (
        '<w:p><w:fldSimple w:instr=" DOCPROPERTY SimpleProp \\* MERGEFORMAT ">'
        '<w:r><w:t>simple cached value</w:t></w:r></w:fldSimple></w:p>'
    )
    complex_field = (
        '<w:p><w:r><w:fldChar w:fldCharType="begin"/></w:r>'
        '<w:r><w:instrText xml:space="preserve"> DOCPROPERTY ComplexProp \\* MERGEFORMAT </w:instrText></w:r>'
        '<w:r><w:fldChar w:fldCharType="separate"/></w:r>'
        '<w:r><w:t>complex cached value</w:t></w:r>'
        '<w:r><w:fldChar w:fldCharType="end"/></w:r></w:p>'
    )
    custom = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" '
        'xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes">'
        '<property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="SimpleProp">'
        '<vt:lpwstr>simple property source</vt:lpwstr></property>'
        '<property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="3" name="ComplexProp">'
        '<vt:lpwstr>complex property source</vt:lpwstr></property>'
        '</Properties>'
    ).encode("utf-8")

    def transform(name: str, data: bytes) -> bytes:
        if name == "word/document.xml":
            return _inject_before_final_sect_pr(data, simple + complex_field)
        if name == "_rels/.rels":
            return _add_relationship(
                data,
                rid="rIdCustomProperties",
                rel_type=RT_CUSTOM_PROPERTIES,
                target="docProps/custom.xml",
            )
        if name == "[Content_Types].xml":
            return _add_overrides(data, [("/docProps/custom.xml", CT_CUSTOM_PROPERTIES)])
        return data

    rewrite_docx(
        SUB_FIXTURE,
        destination,
        transform,
        {"docProps/custom.xml": custom},
    )


def probe_custom_properties(work: Path) -> dict[str, object]:
    sub = work / "custom-properties-sub.docx"
    output = work / "custom-properties-output.docx"
    make_custom_properties_subdoc(sub)
    compose(MAIN_FIXTURE, sub, output)
    document = _xml_part(output, "word/document.xml")
    text_values = document.xpath(".//w:t/text()", namespaces=NS)
    all_field_codes = document.xpath(".//w:fldSimple | .//w:instrText", namespaces=NS)
    custom_parts = sorted(
        name
        for name in _part_names(output)
        if name == "docProps/custom.xml" or name.startswith("docProps/custom")
    )
    root_rels = _xml_part(output, "_rels/.rels")
    custom_rels = root_rels.xpath(
        "pr:Relationship[@Type=$kind]", namespaces=NS, kind=RT_CUSTOM_PROPERTIES
    )
    return {
        "simple_cached_value_kept": "simple cached value" in text_values,
        "complex_cached_value_kept": "complex cached value" in text_values,
        "remaining_docproperty_field_nodes": len(all_field_codes),
        "custom_parts": custom_parts,
        "custom_root_relationships": len(custom_rels),
    }


def _footnotes_xml(*, include_existing: bool) -> bytes:
    existing = ""
    if include_existing:
        existing = (
            '<w:footnote w:id="2"><w:p><w:hyperlink r:id="rId9">'
            '<w:r><w:t>main existing note</w:t></w:r>'
            '</w:hyperlink></w:p></w:footnote>'
        )
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        '<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>'
        '<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>'
        f"{existing}</w:footnotes>"
    ).encode("utf-8")


def make_footnote_subdoc(destination: Path) -> None:
    fragment = (
        '<w:p><w:r><w:t>sub note marker</w:t></w:r><w:r>'
        '<w:footnoteReference w:id="1"/></w:r></w:p>'
    )
    source_footnotes = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        '<w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote>'
        '<w:footnote w:type="continuationSeparator" w:id="0"><w:p><w:r><w:continuationSeparator/></w:r></w:p></w:footnote>'
        '<w:footnote w:id="1"><w:p><w:hyperlink r:id="rId1">'
        '<w:r><w:t>linked source note</w:t></w:r>'
        '</w:hyperlink></w:p></w:footnote></w:footnotes>'
    ).encode("utf-8")
    footnote_rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Id="rId1" Type="{RT_HYPERLINK}" '
        'Target="https://example.test/source-note" TargetMode="External"/>'
        '</Relationships>'
    ).encode("utf-8")

    def transform(name: str, data: bytes) -> bytes:
        if name == "word/document.xml":
            return _inject_before_final_sect_pr(data, fragment)
        if name == "word/_rels/document.xml.rels":
            return _add_relationship(
                data,
                rid="rIdFootnotesOracle",
                rel_type=RT_FOOTNOTES,
                target="footnotes.xml",
            )
        if name == "[Content_Types].xml":
            return _add_overrides(data, [("/word/footnotes.xml", CT_FOOTNOTES)])
        return data

    rewrite_docx(
        SUB_FIXTURE,
        destination,
        transform,
        {
            "word/footnotes.xml": source_footnotes,
            "word/_rels/footnotes.xml.rels": footnote_rels,
        },
    )


def make_main_with_footnotes(destination: Path) -> None:
    footnote_rels = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'<Relationship Id="rId9" Type="{RT_HYPERLINK}" '
        'Target="https://example.test/main-note" TargetMode="External"/>'
        '</Relationships>'
    ).encode("utf-8")

    def transform(name: str, data: bytes) -> bytes:
        if name == "word/_rels/document.xml.rels":
            return _add_relationship(
                data,
                rid="rIdFootnotesOracle",
                rel_type=RT_FOOTNOTES,
                target="footnotes.xml",
            )
        if name == "[Content_Types].xml":
            return _add_overrides(data, [("/word/footnotes.xml", CT_FOOTNOTES)])
        return data

    rewrite_docx(
        MAIN_FIXTURE,
        destination,
        transform,
        {
            "word/footnotes.xml": _footnotes_xml(include_existing=True),
            "word/_rels/footnotes.xml.rels": footnote_rels,
        },
    )


def _footnote_facts(output: Path) -> dict[str, object]:
    document = _xml_part(output, "word/document.xml")
    ids = document.xpath(
        './/w:p[w:r/w:t="sub note marker"]//w:footnoteReference/@w:id', namespaces=NS
    )
    footnotes = _xml_part(output, "word/footnotes.xml")
    copied = footnotes.xpath(
        'w:footnote[.//w:t="linked source note"]', namespaces=NS
    )
    copied_ids = [node.get(f"{{{NS['w']}}}id") for node in copied]
    hyperlink_ids = copied[0].xpath(".//w:hyperlink/@r:id", namespaces=NS) if copied else []
    rels = _relationship_map(output, "word/_rels/footnotes.xml.rels")
    linked = rels.get(hyperlink_ids[0]) if len(hyperlink_ids) == 1 else None
    return {
        "reference_ids": ids,
        "copied_footnote_ids": copied_ids,
        "reference_matches_footnote": len(ids) == 1 and ids == copied_ids,
        "hyperlink_rid": hyperlink_ids,
        "hyperlink_target": linked.get("Target") if linked is not None else None,
        "hyperlink_external": linked is not None
        and linked.get("TargetMode") == "External",
    }


def probe_footnotes(work: Path) -> dict[str, object]:
    sub = work / "footnotes-sub.docx"
    no_main_output = work / "footnotes-no-main-output.docx"
    main = work / "footnotes-main.docx"
    existing_output = work / "footnotes-existing-output.docx"
    make_footnote_subdoc(sub)
    compose(MAIN_FIXTURE, sub, no_main_output)
    make_main_with_footnotes(main)
    compose(main, sub, existing_output)
    return {
        "main_without_footnotes": _footnote_facts(no_main_output),
        "main_with_footnotes": _footnote_facts(existing_output),
    }


def assert_expected(result: dict[str, object]) -> None:
    vml = result["vml"]
    assert vml["relationship_type"] == RT_IMAGE and vml["target_exists"]
    assert vml["image_bytes_match"]

    smartart = result["smartart"]
    assert smartart["rel_ids_count"] == 2 and smartart["four_distinct_targets"]
    for kind in RT_DIAGRAM:
        assert all(smartart[kind].values()), (kind, smartart[kind])

    custom = result["custom_properties"]
    assert custom["simple_cached_value_kept"]
    assert custom["complex_cached_value_kept"]
    assert custom["remaining_docproperty_field_nodes"] == 0
    assert custom["custom_parts"] == []
    assert custom["custom_root_relationships"] == 0

    footnotes = result["footnotes"]
    for facts in footnotes.values():
        assert facts["reference_matches_footnote"]
        assert facts["hyperlink_target"] == "https://example.test/source-note"
        assert facts["hyperlink_external"]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=ROOT / "target/oracle-subdoc-highrisk",
        help="directory for generated DOCX files",
    )
    parser.add_argument(
        "--no-assert",
        action="store_true",
        help="print probe output without enforcing the known 0.20.2/2.2.0 facts",
    )
    args = parser.parse_args()
    work = args.work_dir.resolve()
    work.mkdir(parents=True, exist_ok=True)

    result: dict[str, object] = {
        "docxtpl": docxtpl.__version__,
        "docxcompose": importlib.metadata.version("docxcompose"),
        "vml": probe_vml(work),
        "smartart": probe_smartart(work),
        "custom_properties": probe_custom_properties(work),
        "footnotes": probe_footnotes(work),
    }
    if not args.no_assert:
        assert result["docxtpl"] == "0.20.2", result["docxtpl"]
        assert result["docxcompose"] == "2.2.0", result["docxcompose"]
        assert_expected(result)
    json.dump(result, sys.stdout, ensure_ascii=False, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

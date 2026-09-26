"""Semantic Python oracle for the Subdoc compatibility edge cases.

The probe creates all variants under ``target/oracle-subdoc-compat`` by
default, runs the pinned Python docxtpl/docxcompose implementation, and emits
stable JSON facts.  It deliberately does not print the random ``w:nsid`` value;
only its format and uniqueness are reported.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import zipfile
from pathlib import Path
from typing import Callable

import docxtpl
from docxtpl import DocxTemplate
from lxml import etree


RT_NUMBERING = (
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering"
)
CT_NUMBERING = (
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml"
)
NS = {
    "w": "http://schemas.openxmlformats.org/wordprocessingml/2006/main",
    "m": "http://schemas.openxmlformats.org/officeDocument/2006/math",
    "w14": "http://schemas.microsoft.com/office/word/2010/wordml",
    "wp14": "http://schemas.microsoft.com/office/word/2010/wordprocessingDrawing",
    "pr": "http://schemas.openxmlformats.org/package/2006/relationships",
    "ct": "http://schemas.openxmlformats.org/package/2006/content-types",
}

ROOT = Path(__file__).resolve().parents[2]
MAIN_FIXTURE = ROOT / "tests/fixtures/templates/p6_subdoc_basic.docx"
SUB_FIXTURE = ROOT / "tests/fixtures/templates/p6_subdoc_basic_sub.docx"


def rewrite_docx(
    source: Path,
    destination: Path,
    transform: Callable[[str, bytes], bytes | None],
) -> None:
    destination.parent.mkdir(parents=True, exist_ok=True)
    with zipfile.ZipFile(source) as src, zipfile.ZipFile(
        destination, "w", compression=zipfile.ZIP_DEFLATED
    ) as dst:
        for info in src.infolist():
            data = transform(info.filename, src.read(info.filename))
            if data is not None:
                dst.writestr(info, data)


def rewrite_document(source: Path, destination: Path, mutate: Callable[[str], str]) -> None:
    def transform(name: str, data: bytes) -> bytes:
        if name == "word/document.xml":
            return mutate(data.decode("utf-8")).encode("utf-8")
        return data

    rewrite_docx(source, destination, transform)


def insert_before_final_sect_pr(xml: str, fragment: str) -> str:
    at = xml.rfind("<w:sectPr")
    if at < 0:
        raise AssertionError("fixture has no final w:sectPr")
    return xml[:at] + fragment + xml[at:]


def insert_before_placeholder(xml: str, fragment: str) -> str:
    marker = "<w:p><w:r><w:t>{{p sd }}"
    at = xml.find(marker)
    if at < 0:
        raise AssertionError("fixture has no subdoc placeholder")
    return xml[:at] + fragment + xml[at:]


def set_final_section_type(xml: str, value: str) -> str:
    at = xml.rfind("<w:sectPr")
    if at < 0:
        raise AssertionError("fixture has no final w:sectPr")
    open_end = xml.find(">", at) + 1
    return xml[:open_end] + f'<w:type w:val="{value}"/>' + xml[open_end:]


def remove_main_numbering(source: Path, destination: Path) -> None:
    def transform(name: str, data: bytes) -> bytes | None:
        if name == "word/numbering.xml":
            return None
        if name == "word/_rels/document.xml.rels":
            root = etree.fromstring(data)
            for rel in root.xpath("pr:Relationship[@Type=$kind]", namespaces=NS, kind=RT_NUMBERING):
                rel.getparent().remove(rel)
            return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)
        if name == "[Content_Types].xml":
            root = etree.fromstring(data)
            for override in root.xpath(
                'ct:Override[@PartName="/word/numbering.xml"]', namespaces=NS
            ):
                override.getparent().remove(override)
            return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)
        return data

    rewrite_docx(source, destination, transform)


def read_part(docx: Path, name: str) -> bytes:
    with zipfile.ZipFile(docx) as archive:
        return archive.read(name)


def part_names(docx: Path) -> set[str]:
    with zipfile.ZipFile(docx) as archive:
        return set(archive.namelist())


def xml_part(docx: Path, name: str) -> etree._Element:
    return etree.fromstring(read_part(docx, name))


def compose(main: Path, sub: Path, output: Path) -> str:
    template = DocxTemplate(str(main))
    subdoc = template.new_subdoc(str(sub))
    fragment = str(subdoc)
    template.render({"sd": subdoc})
    template.save(str(output))
    return fragment


def numbered_paragraph() -> str:
    return (
        '<w:p><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:val="1"/>'
        '</w:numPr></w:pPr><w:r><w:t>numbered import</w:t></w:r></w:p>'
    )


def probe_namespaces(work: Path) -> dict[str, object]:
    sub = work / "namespaces-sub.docx"
    output = work / "namespaces-output.docx"
    injected = (
        '<w:p w14:paraId="ABCDEF12"><w:r><m:oMath><m:r><m:t>x</m:t></m:r>'
        '</m:oMath><w14:checkbox><w14:checked w14:val="1"/></w14:checkbox>'
        '<wp14:sizeRelH relativeFrom="margin"><wp14:pctWidth>50000</wp14:pctWidth>'
        '</wp14:sizeRelH></w:r></w:p>'
    )
    rewrite_document(
        SUB_FIXTURE, sub, lambda xml: insert_before_final_sect_pr(xml, injected)
    )
    fragment = compose(MAIN_FIXTURE, sub, output)
    document = xml_part(output, "word/document.xml")
    return {
        "fragment_has_math": "<m:oMath>" in fragment,
        "fragment_has_w14": 'w14:paraId="ABCDEF12"' in fragment,
        "fragment_has_wp14": "<wp14:sizeRelH" in fragment,
        "math_count": len(document.xpath(".//m:oMath", namespaces=NS)),
        "w14_count": len(document.xpath(".//w14:checkbox", namespaces=NS)),
        "wp14_count": len(document.xpath(".//wp14:sizeRelH", namespaces=NS)),
    }


def probe_missing_numbering(work: Path) -> dict[str, object]:
    main = work / "missing-numbering-main.docx"
    sub = work / "missing-numbering-sub.docx"
    output = work / "missing-numbering-output.docx"
    remove_main_numbering(MAIN_FIXTURE, main)
    rewrite_document(
        SUB_FIXTURE,
        sub,
        lambda xml: insert_before_final_sect_pr(xml, numbered_paragraph()),
    )
    compose(main, sub, output)
    relationships = xml_part(output, "word/_rels/document.xml.rels")
    content_types = xml_part(output, "[Content_Types].xml")
    numbering = xml_part(output, "word/numbering.xml")
    return {
        "has_numbering_part": "word/numbering.xml" in part_names(output),
        "numbering_relationships": len(
            relationships.xpath(
                "pr:Relationship[@Type=$kind]", namespaces=NS, kind=RT_NUMBERING
            )
        ),
        "numbering_overrides": len(
            content_types.xpath(
                "ct:Override[@PartName=$part and @ContentType=$content_type]",
                namespaces=NS,
                part="/word/numbering.xml",
                content_type=CT_NUMBERING,
            )
        ),
        "abstract_numberings": len(numbering.xpath("w:abstractNum", namespaces=NS)),
        "numberings": len(numbering.xpath("w:num", namespaces=NS)),
    }


def probe_nsid(work: Path) -> dict[str, object]:
    sub = work / "nsid-sub.docx"
    output = work / "nsid-output.docx"
    rewrite_document(
        SUB_FIXTURE,
        sub,
        lambda xml: insert_before_final_sect_pr(xml, numbered_paragraph()),
    )
    compose(MAIN_FIXTURE, sub, output)
    numbering = xml_part(output, "word/numbering.xml")
    nsids = numbering.xpath("w:abstractNum/w:nsid/@w:val", namespaces=NS)
    return {
        "count": len(nsids),
        "unique": len(nsids) == len(set(nsids)),
        "all_upper_hex8": all(re.fullmatch(r"[0-9A-F]{8}", value) for value in nsids),
    }


def probe_restart(work: Path) -> dict[str, object]:
    sub = work / "restart-sub.docx"
    output = work / "restart-output.docx"
    paragraph = (
        '<w:p><w:pPr><w:pStyle w:val="ListNumber"/></w:pPr>'
        '<w:r><w:t>restart me</w:t></w:r></w:p>'
    )
    rewrite_document(
        SUB_FIXTURE, sub, lambda xml: insert_before_final_sect_pr(xml, paragraph)
    )
    compose(MAIN_FIXTURE, sub, output)
    document = xml_part(output, "word/document.xml")
    numbering = xml_part(output, "word/numbering.xml")
    ids = document.xpath(
        './/w:p[w:r/w:t="restart me"]/w:pPr/w:numPr/w:numId/@w:val',
        namespaces=NS,
    )
    starts: list[str] = []
    if ids:
        starts = numbering.xpath(
            "w:num[@w:numId=$num_id]/w:lvlOverride[@w:ilvl='0']/w:startOverride/@w:val",
            namespaces=NS,
            num_id=ids[0],
        )
    return {
        "paragraph_has_explicit_num_id": len(ids) == 1,
        "start_override_values": starts,
    }


def probe_sections(work: Path) -> dict[str, object]:
    main = work / "sections-main.docx"
    sub = work / "sections-sub.docx"
    output = work / "sections-output.docx"
    main_boundary = (
        '<w:p><w:pPr><w:sectPr><w:type w:val="continuous"/></w:sectPr></w:pPr>'
        '<w:r><w:t>main section one</w:t></w:r></w:p>'
    )
    sub_boundary = (
        '<w:p><w:pPr><w:sectPr><w:type w:val="evenPage"/></w:sectPr></w:pPr>'
        '<w:r><w:t>sub section one</w:t></w:r></w:p>'
    )

    def mutate_main(xml: str) -> str:
        return set_final_section_type(
            insert_before_placeholder(xml, main_boundary), "oddPage"
        )

    def mutate_sub(xml: str) -> str:
        return set_final_section_type(
            insert_before_final_sect_pr(xml, sub_boundary), "continuous"
        )

    rewrite_document(MAIN_FIXTURE, main, mutate_main)
    rewrite_document(SUB_FIXTURE, sub, mutate_sub)
    compose(main, sub, output)
    document = xml_part(output, "word/document.xml")
    sections = document.xpath(
        "w:body/w:p/w:pPr/w:sectPr | w:body/w:sectPr", namespaces=NS
    )
    section_types = []
    for section in sections:
        values = section.xpath("w:type/@w:val", namespaces=NS)
        section_types.append(values[0] if values else "nextPage")
    return {"section_types": section_types}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--work-dir",
        type=Path,
        default=ROOT / "target/oracle-subdoc-compat",
        help="directory for generated DOCX files (must be outside source fixtures)",
    )
    args = parser.parse_args()
    work = args.work_dir.resolve()
    work.mkdir(parents=True, exist_ok=True)

    result = {
        "docxtpl": docxtpl.__version__,
        "namespaces": probe_namespaces(work),
        "missing_numbering": probe_missing_numbering(work),
        "nsid": probe_nsid(work),
        "restart": probe_restart(work),
        "sections": probe_sections(work),
    }
    json.dump(result, sys.stdout, ensure_ascii=False, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

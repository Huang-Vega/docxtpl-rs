#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Canonicalize a .docx into a deterministic JSON fingerprint.

Usage:
    python canonicalize.py <a.docx> <out.json> [--no-normalize]

Canonical form:
- Directory entries (names ending with "/") are skipped.
- parts: {name: {"size", "sha256"}} for every remaining entry (raw zip bytes).
- content_types: children of [Content_Types].xml (Default/Override) as a list
  sorted by Extension / PartName (their order is non-semantic).
- rels: for every *.rels part, Relationship records {owner_part, id, type,
  mode, target} sorted by Id (targets are not resolved).
- xml_sha256: for every *.xml / *.rels part, sha256 over the exclusive C14N
  bytes of the parsed tree.
- document_c14n: the word/document.xml C14N text itself (manual diff aid).

C14N note: lxml 6.x has no top-level etree.c14n(); the equivalent call is
etree.tostring(tree, method="c14n", exclusive=True, with_comments=True),
which is what this module uses.

Normalization (default ON, disable with --no-normalize):
- In docProps/core.xml the *text* of dcterms:created / dcterms:modified is
  replaced with "TS" (timestamps are non-semantic). The part's size/sha256 in
  `parts` and its entry in `xml_sha256` are computed over the normalized C14N
  bytes so both sides compare equal.
"""

import argparse
import hashlib
import json
import posixpath
import sys
import zipfile

from lxml import etree

DCTERMS_NS = "http://purl.org/dc/terms/"
CORE_PART = "docProps/core.xml"
CONTENT_TYPES_PART = "[Content_Types].xml"
DOCUMENT_PART = "word/document.xml"


def _c14n(tree):
    return etree.tostring(tree, method="c14n", exclusive=True, with_comments=True)


def _sha256(data):
    return hashlib.sha256(data).hexdigest()


def _owner_part(rels_name):
    """Owner part of a .rels part, e.g. 'word/_rels/document.xml.rels'
    -> 'word/document.xml'; '_rels/.rels' -> ''."""
    if rels_name == "_rels/.rels":
        return ""
    base_dir = posixpath.dirname(rels_name)      # e.g. word/_rels
    owner_dir = posixpath.dirname(base_dir)      # e.g. word
    file_name = posixpath.basename(rels_name)[: -len(".rels")]
    return posixpath.join(owner_dir, file_name) if owner_dir else file_name


def _normalize_core_timestamps(tree):
    for tag in ("created", "modified"):
        for el in tree.iter("{%s}%s" % (DCTERMS_NS, tag)):
            el.text = "TS"


def _is_xml_part(name):
    return name.endswith(".xml") or name.endswith(".rels")


def canonicalize_docx(path, normalize=True):
    result = {
        "normalize": normalize,
        "parts": {},
        "content_types": [],
        "rels": {},
        "xml_sha256": {},
        "document_c14n": None,
    }
    with zipfile.ZipFile(path) as zf:
        for name in sorted(zf.namelist()):
            if name.endswith("/"):
                continue  # directory entry
            data = zf.read(name)

            if name == CORE_PART and normalize:
                tree = etree.fromstring(data)
                _normalize_core_timestamps(tree)
                c14n = _c14n(tree)
                result["parts"][name] = {"size": len(c14n), "sha256": _sha256(c14n)}
                result["xml_sha256"][name] = _sha256(c14n)
                continue

            result["parts"][name] = {"size": len(data), "sha256": _sha256(data)}

            if name == CONTENT_TYPES_PART:
                entries = []
                for child in etree.fromstring(data):
                    local = etree.QName(child).localname
                    if local == "Default":
                        entries.append((
                            child.get("Extension") or "",
                            {"kind": "Default", "extension": child.get("Extension"),
                             "content_type": child.get("ContentType")},
                        ))
                    elif local == "Override":
                        entries.append((
                            child.get("PartName") or "",
                            {"kind": "Override", "part_name": child.get("PartName"),
                             "content_type": child.get("ContentType")},
                        ))
                    else:
                        entries.append(("", {"kind": local,
                                              "attributes": dict(child.attrib)}))
                entries.sort(key=lambda pair: pair[0])
                result["content_types"] = [entry for _, entry in entries]
            elif name.endswith(".rels"):
                rels = []
                for rel in etree.fromstring(data):
                    rels.append({
                        "owner_part": _owner_part(name),
                        "id": rel.get("Id"),
                        "type": rel.get("Type"),
                        "mode": rel.get("Mode"),
                        "target": rel.get("Target"),
                    })
                rels.sort(key=lambda rel: rel["id"] or "")
                result["rels"][name] = rels

            if _is_xml_part(name):
                c14n = _c14n(etree.fromstring(data))
                result["xml_sha256"][name] = _sha256(c14n)
                if name == DOCUMENT_PART:
                    result["document_c14n"] = c14n.decode("utf-8")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__.strip())
    parser.add_argument("docx", help="path to the .docx to canonicalize")
    parser.add_argument("out_json", help="output JSON path")
    parser.add_argument("--no-normalize", action="store_true",
                        help="disable docProps/core.xml timestamp normalization")
    args = parser.parse_args()

    data = canonicalize_docx(args.docx, normalize=not args.no_normalize)
    with open(args.out_json, "w", encoding="utf-8") as fh:
        json.dump(data, fh, indent=2, ensure_ascii=False)
        fh.write("\n")
    return 0


if __name__ == "__main__":
    sys.exit(main())

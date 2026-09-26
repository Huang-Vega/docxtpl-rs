#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Deep-compare two .docx files through their canonical forms.

Usage:
    python compare.py <expected.docx> <actual.docx> [--no-normalize]

Both files are canonicalized (see canonicalize.py; docProps/core.xml
timestamps are normalized unless --no-normalize is given) and then deeply
compared: part name sets, per-part sha256, [Content_Types].xml lists,
rels collections and per-XML-part C14N hashes.

On equality prints "MATCH" and exits 0. Otherwise prints a JSON difference
report (first mismatching part with a content summary, plus up to 50
differences; for word/document.xml the report includes an excerpt of the two
C14N texts around the first differing character) and exits 1.
"""

import argparse
import json
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import canonicalize

CONTENT_TYPES_KEY = "[Content_Types].xml"
DOCUMENT_KEY = "word/document.xml"


def _preview(value, limit=300):
    if value is None:
        return None
    if isinstance(value, str):
        text = value
    else:
        try:
            text = json.dumps(value, ensure_ascii=False)
        except (TypeError, ValueError):
            text = repr(value)
    return text if len(text) <= limit else text[:limit] + "…"


def _first_char_diff(a, b):
    for i, (ca, cb) in enumerate(zip(a, b)):
        if ca != cb:
            return i
    if len(a) != len(b):
        return min(len(a), len(b))
    return None


def _excerpt(text, pos, radius=80):
    lo = max(0, pos - radius)
    hi = min(len(text), pos + radius)
    prefix = "…" if lo > 0 else ""
    suffix = "…" if hi < len(text) else ""
    return prefix + text[lo:hi] + suffix


def _document_note(expected, actual):
    """Content summary for a differing word/document.xml."""
    exp_c14n = expected.get("document_c14n") or ""
    act_c14n = actual.get("document_c14n") or ""
    pos = _first_char_diff(exp_c14n, act_c14n)
    if pos is None:
        return None
    return ("first c14n char diff at %d -- expected: %r | actual: %r"
            % (pos, _excerpt(exp_c14n, pos), _excerpt(act_c14n, pos)))


def diff_canonical(expected, actual):
    diffs = []

    def add(section, key, exp, act, note=None):
        diffs.append({
            "section": section,
            "key": key,
            "expected": _preview(exp),
            "actual": _preview(act),
            "note": note,
        })

    e_parts, a_parts = expected["parts"], actual["parts"]
    for name in sorted(set(e_parts) | set(a_parts)):
        if name not in a_parts:
            add("parts", name, e_parts[name], "<missing part>")
        elif name not in e_parts:
            add("parts", name, "<unexpected part>", a_parts[name])
        elif e_parts[name] != a_parts[name]:
            note = _document_note(expected, actual) if name == DOCUMENT_KEY else None
            add("parts", name, e_parts[name], a_parts[name], note)

    if expected["content_types"] != actual["content_types"]:
        add("content_types", CONTENT_TYPES_KEY,
            expected["content_types"], actual["content_types"])

    e_rels, a_rels = expected["rels"], actual["rels"]
    for name in sorted(set(e_rels) | set(a_rels)):
        if name not in a_rels:
            add("rels", name, e_rels[name], "<missing rels part>")
        elif name not in e_rels:
            add("rels", name, "<unexpected rels part>", a_rels[name])
        elif e_rels[name] != a_rels[name]:
            add("rels", name, e_rels[name], a_rels[name])

    e_xml, a_xml = expected["xml_sha256"], actual["xml_sha256"]
    for name in sorted(set(e_xml) | set(a_xml)):
        if name not in a_xml:
            add("xml_sha256", name, e_xml[name], "<missing xml part>")
        elif name not in e_xml:
            add("xml_sha256", name, "<unexpected xml part>", a_xml[name])
        elif e_xml[name] != a_xml[name]:
            note = _document_note(expected, actual) if name == DOCUMENT_KEY else None
            add("xml_sha256", name, e_xml[name], a_xml[name], note)

    return diffs


def main():
    parser = argparse.ArgumentParser(description=__doc__.strip())
    parser.add_argument("expected_docx")
    parser.add_argument("actual_docx")
    parser.add_argument("--no-normalize", action="store_true",
                        help="disable docProps/core.xml timestamp normalization")
    args = parser.parse_args()

    normalize = not args.no_normalize
    expected = canonicalize.canonicalize_docx(args.expected_docx, normalize=normalize)
    actual = canonicalize.canonicalize_docx(args.actual_docx, normalize=normalize)
    diffs = diff_canonical(expected, actual)

    if not diffs:
        print("MATCH")
        return 0

    report = {
        "result": "MISMATCH",
        "expected_docx": args.expected_docx,
        "actual_docx": args.actual_docx,
        "difference_count": len(diffs),
        "first_difference": diffs[0],
        "differences": diffs[:50],
    }
    print(json.dumps(report, indent=2, ensure_ascii=False))
    return 1


if __name__ == "__main__":
    sys.exit(main())

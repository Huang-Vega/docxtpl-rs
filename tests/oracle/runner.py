#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""Run the Python docxtpl oracle over all fixtures.

Usage:
    python tests/oracle/runner.py [--only <fixture_id>]

Reads tests/fixtures/manifest.json. For each fixture:
- mode=render:    DocxTemplate(template).render(context); tpl.save(out)
- mode=roundtrip: Document(template).save(out)

Any exception is caught and recorded (status="error", with the exception type
name and message); a fixture error is an expected *outcome*, not a failure of
this script. Successful fixtures are written to
tests/oracle/expected/<id>.docx. A summary of all statuses goes to
tests/oracle/expected/report.json (id/mode/status/error_type/error_message
truncated to 500 chars, plus version fingerprint and generation time).

Always exits 0 when fixtures were run (exits 1 only for a bad --only id).
"""

import argparse
import json
import sys
from datetime import datetime
from importlib.metadata import version
from pathlib import Path

from docx import Document
from docxtpl import DocxTemplate

SCRIPT_DIR = Path(__file__).resolve().parent          # tests/oracle
ROOT = SCRIPT_DIR.parents[1]                          # project root
FIXTURES_DIR = ROOT / "tests" / "fixtures"
EXPECTED_DIR = SCRIPT_DIR / "expected"
MANIFEST_PATH = FIXTURES_DIR / "manifest.json"


def run_fixture(fx):
    template_path = FIXTURES_DIR / fx["template"]
    out_path = EXPECTED_DIR / (fx["id"] + ".docx")
    result = {
        "id": fx["id"],
        "mode": fx["mode"],
        "status": "ok",
        "error_type": None,
        "error_message": None,
    }
    try:
        if fx["mode"] == "render":
            context = {}
            if fx.get("context"):
                context = json.loads(
                    (FIXTURES_DIR / fx["context"]).read_text(encoding="utf-8"))
            tpl = DocxTemplate(str(template_path))
            tpl.render(context)
            tpl.save(str(out_path))
        else:
            Document(str(template_path)).save(str(out_path))
    except Exception as exc:  # noqa: BLE001 -- the oracle records upstream truth
        result["status"] = "error"
        result["error_type"] = type(exc).__name__
        result["error_message"] = str(exc)[:500]
        if out_path.exists():
            out_path.unlink()  # drop any stale output from previous runs
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__.strip())
    parser.add_argument("--only", metavar="ID",
                        help="run only the fixture with this id")
    args = parser.parse_args()

    manifest = json.loads(MANIFEST_PATH.read_text(encoding="utf-8"))
    fixtures = manifest["fixtures"]
    if args.only:
        fixtures = [fx for fx in fixtures if fx["id"] == args.only]
        if not fixtures:
            print("no fixture with id %r in %s" % (args.only, MANIFEST_PATH),
                  file=sys.stderr)
            return 1

    EXPECTED_DIR.mkdir(parents=True, exist_ok=True)
    results = [run_fixture(fx) for fx in fixtures]

    report = {
        "generated_at": datetime.now().astimezone().isoformat(timespec="seconds"),
        "versions": {p: version(p) for p in ("docxtpl", "jinja2", "python-docx", "lxml")},
        "fixtures": results,
    }
    (EXPECTED_DIR / "report.json").write_text(
        json.dumps(report, indent=2, ensure_ascii=False) + "\n",
        encoding="utf-8")

    errors = [r for r in results if r["status"] == "error"]
    print("ran %d fixtures: %d ok, %d error -> %s"
          % (len(results), len(results) - len(errors), len(errors),
             EXPECTED_DIR / "report.json"))
    for r in errors:
        print("  ERROR %s [%s]: %s: %s"
              % (r["id"], r["mode"], r["error_type"], r["error_message"]))
    return 0


if __name__ == "__main__":
    sys.exit(main())

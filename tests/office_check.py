#!/usr/bin/env python3
"""Render representative fixtures, reopen/resave them in LibreOffice, verify tables."""

import json
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

from lxml import etree


ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "tests" / "fixtures"
CASES = {
    "r2_var_basic": (0, 0, 0),
    "r3_tr_for": (1, 3, 6),
    "r3_tr_for_empty": (1, 1, 2),
    "r3_tc_for": (1, 1, 3),
    "r3_vm": (1, 4, 8),
    "r3_hm": (1, 3, 8),
    "r3_nested_tables": (3, 6, 6),
    "r3_combo_invoice": (1, 3, 9),
}
NS = {"w": "http://schemas.openxmlformats.org/wordprocessingml/2006/main"}


def main() -> int:
    cli = ROOT / "target" / "debug" / ("docxtpl.exe" if sys.platform == "win32" else "docxtpl")
    soffice = shutil.which("soffice") or shutil.which("libreoffice")
    if not cli.is_file() or not soffice:
        print("Build docxtpl-cli and install LibreOffice first", file=sys.stderr)
        return 2
    manifest = {x["id"]: x for x in json.loads((FIXTURES / "manifest.json").read_text())["fixtures"]}
    with tempfile.TemporaryDirectory(prefix="docxtpl-office-") as tmp:
        base = Path(tmp)
        rendered, resaved = base / "rendered", base / "resaved"
        rendered.mkdir()
        resaved.mkdir()
        for name in CASES:
            fx = manifest[name]
            subprocess.run([str(cli), "render", str(FIXTURES / fx["template"]),
                            str(FIXTURES / fx["context"]), str(rendered / f"{name}.docx")],
                           check=True, capture_output=True)
        command = [soffice, f"-env:UserInstallation={(base / 'profile').as_uri()}",
                   "--headless", "--convert-to", "docx", "--outdir", str(resaved)]
        command += [str(rendered / f"{name}.docx") for name in CASES]
        subprocess.run(command, check=True, capture_output=True, timeout=120)
        for name, expected in CASES.items():
            path = resaved / f"{name}.docx"
            if not path.is_file():
                raise AssertionError(f"LibreOffice did not resave {name}")
            with zipfile.ZipFile(path) as archive:
                root = etree.fromstring(archive.read("word/document.xml"))
            actual = tuple(len(root.xpath(f".//w:{tag}", namespaces=NS))
                           for tag in ("tbl", "tr", "tc"))
            if actual != expected:
                raise AssertionError(f"{name}: expected {expected}, got {actual}")
            print(f"{name}: reopened and resaved, tables/rows/cells = {actual}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

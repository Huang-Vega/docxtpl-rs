#!/usr/bin/env python3
"""Fail the release gate on missing or explicitly disallowed dependency licenses."""

from __future__ import annotations

import json
import os
from pathlib import Path
import re
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
DISALLOWED = re.compile(r"\b(?:AGPL|SSPL|BUSL|GPL-(?:2|3)\.0)|COMMONS-CLAUSE")


def main() -> int:
    env = os.environ.copy()
    env.setdefault("CARGO_TARGET_DIR", str(ROOT / "target"))
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--format-version", "1"],
        cwd=ROOT,
        env=env,
        check=True,
        text=True,
        encoding="utf-8",
        stdout=subprocess.PIPE,
    )
    metadata = json.loads(result.stdout)
    failures: list[str] = []
    audited: list[dict[str, str]] = []
    for package in sorted(metadata["packages"], key=lambda item: (item["name"], item["version"])):
        if package["source"] is None:
            continue
        license_expression = package.get("license") or ""
        record = {
            "name": package["name"],
            "version": package["version"],
            "license": license_expression,
        }
        audited.append(record)
        upper = license_expression.upper()
        if not license_expression:
            failures.append(f"{package['name']} {package['version']}: missing license metadata")
        elif DISALLOWED.search(upper):
            failures.append(
                f"{package['name']} {package['version']}: disallowed license {license_expression}"
            )

    report_path = ROOT / "target" / "license-audit.json"
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(
        json.dumps({"packages": audited, "failures": failures}, indent=2) + "\n",
        encoding="utf-8",
    )
    if failures:
        print("\n".join(failures), file=sys.stderr)
        return 1
    print(f"license audit passed: {len(audited)} registry packages")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

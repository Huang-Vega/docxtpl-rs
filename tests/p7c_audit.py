#!/usr/bin/env python3
"""P7c corpus resource audit and Rust per-stage performance regression
baseline.

By default only the current measurement is printed; --write-baseline writes
the results to docs/p7c-performance-baseline.json. When a baseline exists,
--compare returns nonzero if the median regresses by more than 20% on the
same platform/sample. Builds, temporary output and caches all live inside the
project directory to avoid polluting the system drive.
"""

from __future__ import annotations

import argparse
import json
import platform
import subprocess
import sys
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
TEMPLATES = ROOT / "tests" / "fixtures" / "templates"
CONTEXTS = ROOT / "tests" / "fixtures" / "contexts"
BASELINE = ROOT / "docs" / "p7c-performance-baseline.json"
CASES = ("r2_var_basic", "r3_combo_invoice", "p7b_dynamic_table")


def corpus_metrics() -> dict[str, object]:
    maxima = {"compressed_bytes": (0, ""), "entries": (0, ""),
              "entry_uncompressed_bytes": (0, ""), "total_uncompressed_bytes": (0, ""),
              "compression_ratio": (0.0, "")}
    files = sorted(TEMPLATES.glob("*.docx"))
    for path in files:
        with zipfile.ZipFile(path) as archive:
            infos = archive.infolist()
            values = {
                "compressed_bytes": path.stat().st_size,
                "entries": len(infos),
                "entry_uncompressed_bytes": max((i.file_size for i in infos), default=0),
                "total_uncompressed_bytes": sum(i.file_size for i in infos),
                "compression_ratio": max(
                    (i.file_size / i.compress_size for i in infos if i.compress_size), default=0.0
                ),
            }
            for key, value in values.items():
                if value > maxima[key][0]:
                    maxima[key] = (value, path.name)
    return {"template_count": len(files), "maxima": {
        key: {"value": round(value, 3), "fixture": fixture}
        for key, (value, fixture) in maxima.items()
    }}


def executable() -> Path:
    name = "phase_bench.exe" if sys.platform == "win32" else "phase_bench"
    path = ROOT / "target" / "release" / "examples" / name
    if not path.exists():
        subprocess.run(["cargo", "build", "--release", "-p", "docxtpl-rs", "--example", "phase_bench"], cwd=ROOT, check=True)
    return path


def benchmark(iterations: int) -> dict[str, object]:
    exe = executable()
    results: dict[str, object] = {}
    for case in CASES:
        template = TEMPLATES / f"{case}.docx"
        context = CONTEXTS / f"{case}.json"
        if not context.exists():
            raise SystemExit(f"benchmark cases must use a JSON context, missing {context}")
        completed = subprocess.run(
            [str(exe), str(template), str(context), str(iterations)], cwd=ROOT,
            check=True, capture_output=True, text=True)
        results[case] = json.loads(completed.stdout)
    return results


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--iterations", type=int, default=15)
    parser.add_argument("--write-baseline", action="store_true")
    parser.add_argument("--compare", action="store_true")
    args = parser.parse_args()
    if args.iterations < 3:
        parser.error("--iterations must be at least 3")

    report = {
        "schema": 2,
        "platform": {"system": platform.system(), "machine": platform.machine(),
                     "python": platform.python_version()},
        "corpus": corpus_metrics(),
        "benchmarks": benchmark(args.iterations),
        "regression_threshold_percent": 20,
    }
    print(json.dumps(report, ensure_ascii=False, indent=2))

    if args.compare:
        baseline = json.loads(BASELINE.read_text(encoding="utf-8"))
        baseline_platform = baseline.get("platform", {})
        current_platform = report["platform"]
        if any(
            baseline_platform.get(key) != current_platform.get(key)
            for key in ("system", "machine")
        ):
            print("performance baseline platform mismatch; refusing cross-platform comparison", file=sys.stderr)
            return 2
        regressions = []
        for case, current in report["benchmarks"].items():
            previous = baseline.get("benchmarks", {}).get(case)
            if previous and "render_ms" in previous and current["render_ms"] > previous["render_ms"] * 1.2:
                regressions.append(
                    f"{case}: {previous['render_ms']}ms -> {current['render_ms']}ms"
                )
        if regressions:
            print("performance regression over 20%: " + "; ".join(regressions), file=sys.stderr)
            return 1
    if args.write_baseline:
        BASELINE.write_text(json.dumps(report, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

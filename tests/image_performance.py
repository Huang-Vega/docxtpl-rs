#!/usr/bin/env python3
"""Benchmark the pinned Python docxtpl image pipeline for Rust comparisons."""

from __future__ import annotations

import argparse
import ctypes
from ctypes import wintypes
import hashlib
import json
from pathlib import Path
import sys
import time

from docx.image.image import Image
from docxtpl import DocxTemplate, InlineImage


IMAGE_EXTENSIONS = {".png", ".jpg", ".jpeg", ".gif", ".bmp", ".tif", ".tiff"}


def image_paths(source: Path, count: int) -> list[Path]:
    if source.is_file():
        if source.suffix.lower() == ".txt":
            paths = [Path(line) for line in source.read_text(encoding="utf-8").splitlines() if line]
        else:
            paths = [source]
    else:
        paths = sorted(
            path
            for path in source.rglob("*")
            if path.is_file() and path.suffix.lower() in IMAGE_EXTENSIONS
        )
    if not paths:
        raise ValueError("IMAGE_SOURCE contains no supported images")
    compatible = []
    digests = set()
    for path in paths:
        try:
            Image.from_file(str(path))
        except Exception:  # noqa: BLE001 - skip inputs rejected by the pinned oracle
            continue
        digest = hashlib.sha1(path.read_bytes()).digest()
        if digest in digests:
            continue
        digests.add(digest)
        compatible.append(path)
        if len(compatible) == count:
            break
    if len(compatible) < count:
        raise ValueError(f"only {len(compatible)} Python-compatible images are available")
    return compatible


def peak_rss_bytes() -> int:
    if sys.platform != "win32":
        return 0

    class ProcessMemoryCounters(ctypes.Structure):
        _fields_ = [
            ("cb", wintypes.DWORD),
            ("PageFaultCount", wintypes.DWORD),
            ("PeakWorkingSetSize", ctypes.c_size_t),
            ("WorkingSetSize", ctypes.c_size_t),
            ("QuotaPeakPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPagedPoolUsage", ctypes.c_size_t),
            ("QuotaPeakNonPagedPoolUsage", ctypes.c_size_t),
            ("QuotaNonPagedPoolUsage", ctypes.c_size_t),
            ("PagefileUsage", ctypes.c_size_t),
            ("PeakPagefileUsage", ctypes.c_size_t),
        ]

    counters = ProcessMemoryCounters()
    counters.cb = ctypes.sizeof(counters)
    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    psapi = ctypes.WinDLL("psapi", use_last_error=True)
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    psapi.GetProcessMemoryInfo.argtypes = [
        wintypes.HANDLE,
        ctypes.POINTER(ProcessMemoryCounters),
        wintypes.DWORD,
    ]
    psapi.GetProcessMemoryInfo.restype = wintypes.BOOL
    process = kernel32.GetCurrentProcess()
    ok = psapi.GetProcessMemoryInfo(
        process, ctypes.byref(counters), counters.cb
    )
    return int(counters.PeakWorkingSetSize) if ok else 0


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("template", type=Path)
    parser.add_argument("image_source", type=Path)
    parser.add_argument("count", type=int)
    parser.add_argument("iterations", type=int)
    parser.add_argument("output", type=Path)
    parser.add_argument("--manifest", type=Path)
    args = parser.parse_args()
    if args.count <= 0 or args.iterations <= 0:
        parser.error("count and iterations must be greater than zero")

    paths = image_paths(args.image_source, args.count)
    if args.manifest is not None:
        args.manifest.parent.mkdir(parents=True, exist_ok=True)
        args.manifest.write_text(
            "".join(f"{path.resolve()}\n" for path in paths), encoding="utf-8"
        )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    totals = {"context_build_ns": 0, "open_ns": 0, "render_ns": 0, "write_ns": 0}

    for _ in range(args.iterations):
        started = time.perf_counter_ns()
        template = DocxTemplate(args.template)
        totals["open_ns"] += time.perf_counter_ns() - started

        started = time.perf_counter_ns()
        images = [InlineImage(template, str(path)) for path in paths]
        rows = [
            {"n": f"row-{index}", "img": images[index % len(images)]}
            for index in range(args.count)
        ]
        totals["context_build_ns"] += time.perf_counter_ns() - started

        started = time.perf_counter_ns()
        template.render({"rows": rows})
        totals["render_ns"] += time.perf_counter_ns() - started

        started = time.perf_counter_ns()
        template.save(args.output)
        totals["write_ns"] += time.perf_counter_ns() - started

    divisor = args.iterations * 1_000_000
    result = {
        "runtime": "python",
        "iterations": args.iterations,
        "image_count": args.count,
        "source_image_count": len(paths),
        "context_build_ms": totals["context_build_ns"] / divisor,
        "open_ms": totals["open_ns"] / divisor,
        "render_ms": totals["render_ns"] / divisor,
        "write_ms": totals["write_ns"] / divisor,
        "output_bytes": args.output.stat().st_size,
        "peak_rss_bytes": peak_rss_bytes(),
    }
    print(json.dumps(result, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())

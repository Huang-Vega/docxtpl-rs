#!/usr/bin/env python3
"""Build and run the library and CLI solely from packaged crate archives."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import zipfile


ROOT = Path(__file__).resolve().parents[1]
CRATES = (
    "docxtpl-compat",
    "docxtpl-opc",
    "docxtpl-rich",
    "docxtpl-xml",
    "docxtpl-template",
    "docxtpl-rs",
    "docxtpl-cli",
)


def run(command: list[str], *, cwd: Path, env: dict[str, str]) -> None:
    print("+", " ".join(command))
    subprocess.run(command, cwd=cwd, env=env, check=True)


def workspace_version() -> str:
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=ROOT,
        check=True,
        text=True,
        encoding="utf-8",
        stdout=subprocess.PIPE,
    )
    packages = json.loads(result.stdout)["packages"]
    versions = {package["version"] for package in packages}
    if len(versions) != 1:
        raise RuntimeError(f"workspace versions differ: {sorted(versions)}")
    return versions.pop()


def validate_docx(path: Path) -> None:
    with zipfile.ZipFile(path) as archive:
        document = archive.read("word/document.xml").decode("utf-8")
    if "World" not in document:
        raise RuntimeError(f"{path} does not contain rendered value")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--package-dir", type=Path, default=ROOT / "target" / "package")
    args = parser.parse_args()
    version = workspace_version()
    smoke = (ROOT / "target" / "release-smoke").resolve()
    if smoke.exists():
        shutil.rmtree(smoke)
    packages = smoke / "packages"
    packages.mkdir(parents=True)
    workspace_members = ["sample", *(f"packages/{name}-{version}" for name in CRATES)]
    (smoke / "Cargo.toml").write_text(
        "[workspace]\nresolver = \"2\"\nmembers = ["
        + ", ".join(json.dumps(member) for member in workspace_members)
        + "]\n",
        encoding="utf-8",
    )

    extracted: dict[str, Path] = {}
    for name in CRATES:
        archive = args.package_dir / f"{name}-{version}.crate"
        if not archive.is_file():
            raise FileNotFoundError(archive)
        with tarfile.open(archive, "r:gz") as tar:
            tar.extractall(packages, filter="data")
        extracted[name] = (packages / f"{name}-{version}").resolve()
        for required in ("LICENSE", "README.md"):
            if not (extracted[name] / required).is_file():
                raise RuntimeError(f"{archive.name} is missing {required}")

    cargo_dir = smoke / ".cargo"
    cargo_dir.mkdir()
    patch_lines = ["[patch.crates-io]"]
    for name, path in extracted.items():
        patch_lines.append(f"{name} = {{ path = {json.dumps(path.as_posix())} }}")
    (cargo_dir / "config.toml").write_text("\n".join(patch_lines) + "\n", encoding="utf-8")

    sample = smoke / "sample"
    (sample / "src").mkdir(parents=True)
    (sample / "Cargo.toml").write_text(
        "[package]\nname = \"docxtpl-release-smoke\"\nversion = \"0.0.0\"\n"
        "edition = \"2021\"\npublish = false\n\n[dependencies]\n"
        f"docxtpl-rs = \"={version}\"\nserde_json = \"1\"\n",
        encoding="utf-8",
    )
    (sample / "src" / "main.rs").write_text(
        "use docxtpl_rs::{DocxTemplate, RenderOptions};\n"
        "use serde_json::json;\n"
        "fn main() -> Result<(), Box<dyn std::error::Error>> {\n"
        "    let args: Vec<_> = std::env::args_os().collect();\n"
        "    let tpl = DocxTemplate::open(&args[1])?;\n"
        "    tpl.render(&json!({\"name\": \"World\"}), &RenderOptions::compat())?\n"
        "        .save(&args[2])?;\n"
        "    Ok(())\n}\n",
        encoding="utf-8",
    )

    env = os.environ.copy()
    env["CARGO_TARGET_DIR"] = str(smoke / "target")
    template = ROOT / "tests" / "fixtures" / "templates" / "r2_var_basic.docx"
    context = ROOT / "tests" / "fixtures" / "contexts" / "r2_var_basic.json"
    library_output = smoke / "library-output.docx"
    run(["cargo", "generate-lockfile", "--offline"], cwd=sample, env=env)
    run(
        ["cargo", "run", "--locked", "--offline", "--", str(template), str(library_output)],
        cwd=sample,
        env=env,
    )
    validate_docx(library_output)

    install_root = smoke / "install"
    run(
        [
            "cargo",
            "install",
            "--path",
            str(extracted["docxtpl-cli"]),
            "--root",
            str(install_root),
            "--locked",
            "--offline",
            "--force",
        ],
        cwd=smoke,
        env=env,
    )
    executable = install_root / "bin" / ("docxtpl.exe" if os.name == "nt" else "docxtpl")
    cli_output = smoke / "cli-output.docx"
    run([str(executable), "-q", str(template), str(context), str(cli_output)], cwd=smoke, env=env)
    validate_docx(cli_output)
    print(f"release smoke passed for {version}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"release smoke failed: {error}", file=sys.stderr)
        raise SystemExit(1) from error

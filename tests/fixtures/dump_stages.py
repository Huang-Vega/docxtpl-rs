"""Export golden intermediates of each render stage, for fixture-by-fixture
comparison during the Rust port (P0 oracle infrastructure).

For every fixture with mode=render in the manifest this produces:
  tests/fixtures/stages/<id>.full_patched.xml
      = upstream patch_xml(raw word/document.xml inside the template)
        -- pins the regex port of docxtpl-compat exactly (whole-document
        input scope).
  tests/fixtures/stages/<id>.body_patched.xml
      = upstream patch_xml(lxml.tostring(body)), i.e. the string actually
        fed into jinja.
  tests/fixtures/stages/<id>.pre_recover.xml
      = output of upstream render_xml_part (jinja render + newline
        restoration + resolve_listing), before the recover parse of
        fix_tables.
  tests/fixtures/stages/<id>.recovered.xml
      = etree.fromstring(pre_recover, XMLParser(recover=True)) followed by
        tostring -- pins the lenient (recover) parser of docxtpl-xml exactly.
Fixtures expected to error (r2_syntax_error, p4_img_bad) only export the
full/body patched stages.
Fixtures with context_kind=python build their typed context via
build_context(tpl) (RichText/Listing/InlineImage etc., see P4).
"""
import importlib.util
import json
import os
import sys
import zipfile

from docxtpl import DocxTemplate
from lxml import etree

ROOT = os.path.dirname(os.path.abspath(__file__))
TPL_DIR = os.path.join(ROOT, "templates")
CTX_DIR = os.path.join(ROOT, "contexts")
OUT_DIR = os.path.join(ROOT, "stages")


def write(name, text):
    if isinstance(text, bytes):
        text = text.decode("utf-8")
    with open(os.path.join(OUT_DIR, name), "w", encoding="utf-8", newline="") as fh:
        fh.write(text)


def load_context(fx, tpl):
    if not fx.get("context"):
        return {}
    ctx_path = os.path.join(ROOT, fx["context"].replace("/", os.sep))
    if fx.get("context_kind") == "python":
        spec = importlib.util.spec_from_file_location(
            "docxtplrs_stage_ctx_" + fx["id"], ctx_path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module.build_context(tpl)
    with open(ctx_path, encoding="utf-8") as fh:
        return json.load(fh)


def main():
    os.makedirs(OUT_DIR, exist_ok=True)
    with open(os.path.join(ROOT, "manifest.json"), encoding="utf-8") as fh:
        manifest = json.load(fh)

    summary = []
    for fx in manifest["fixtures"]:
        if fx["mode"] != "render":
            continue
        fid = fx["id"]
        tpl_path = os.path.join(ROOT, fx["template"].replace("/", os.sep))
        with zipfile.ZipFile(tpl_path) as zf:
            raw_doc_xml = zf.read("word/document.xml").decode("utf-8")

        tpl = DocxTemplate.__new__(DocxTemplate)
        full_patched = tpl.patch_xml(raw_doc_xml)
        write(f"{fid}.full_patched.xml", full_patched)

        # Use the same body extraction path as build_xml
        real = DocxTemplate(tpl_path)
        doc = real.get_docx()
        body_xml = real.xml_to_string(doc._element.body)
        body_patched = real.patch_xml(body_xml)
        write(f"{fid}.body_patched.xml", body_patched)

        ctx_rel = fx.get("context")
        if ctx_rel:
            ctx = load_context(fx, real)
        else:
            ctx = {}

        try:
            pre = real.render_xml_part(body_patched, doc._part, ctx)
        except Exception as exc:  # noqa: BLE001 - expected only for r2_syntax_error
            summary.append((fid, "error", type(exc).__name__))
            continue
        write(f"{fid}.pre_recover.xml", pre)

        parser = etree.XMLParser(recover=True)
        tree = etree.fromstring(pre.encode("utf-8"), parser=parser)
        recovered = etree.tostring(tree, encoding="unicode") if tree is not None else ""
        write(f"{fid}.recovered.xml", recovered)
        summary.append((fid, "ok", ""))

    for fid, status, err in summary:
        print(f"{fid}: {status} {err}")
    return 0


if __name__ == "__main__":
    sys.exit(main())

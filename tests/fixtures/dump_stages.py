"""导出各渲染阶段的 golden 中间产物，供 Rust 移植做逐 fixture 对照（P0 oracle 基础设施）。

对 manifest 中 mode=render 的每个 fixture 产出：
  tests/fixtures/stages/<id>.full_patched.xml
      = 上游 patch_xml(模板内 word/document.xml 原文)
        —— 精确钉死 docxtpl-compat 的正则移植（全文档输入口径）。
  tests/fixtures/stages/<id>.body_patched.xml
      = 上游 patch_xml(lxml.tostring(body))，即真正送进 jinja 的字符串。
  tests/fixtures/stages/<id>.pre_recover.xml
      = 上游 render_xml_part 的输出（jinja 渲染 + 换行还原 + resolve_listing，
        尚未经 fix_tables 的 recover 解析）。
  tests/fixtures/stages/<id>.recovered.xml
      = etree.fromstring(pre_recover, XMLParser(recover=True)) 再 tostring 的结果
        —— 精确钉死 docxtpl-xml 宽松(recover)解析器。
error 预期的 fixture（r2_syntax_error）只导出 full/body patched。
"""
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

        # 走与 build_xml 相同的 body 提取口径
        real = DocxTemplate(tpl_path)
        doc = real.get_docx()
        body_xml = real.xml_to_string(doc._element.body)
        body_patched = real.patch_xml(body_xml)
        write(f"{fid}.body_patched.xml", body_patched)

        ctx_rel = fx.get("context")
        if ctx_rel:
            ctx_path = os.path.join(ROOT, ctx_rel.replace("/", os.sep))
            with open(ctx_path, encoding="utf-8") as fh:
                ctx = json.load(fh)
        else:
            ctx = {}

        try:
            pre = real.render_xml_part(body_patched, doc._part, ctx)
        except Exception as exc:  # noqa: BLE001 - 仅 r2_syntax_error 预期
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

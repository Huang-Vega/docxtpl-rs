# -*- coding: utf-8 -*-
"""P4 fixture context（由 tests/fixtures/generate.py 生成，勿手改）。

提供 build_context(tpl)：返回渲染上下文，可包含 RichText/RichTextParagraph/
Listing/InlineImage/Subdoc 等类型化值（对齐上游 docxtpl 0.20.2 用法）。
"""
import io
import os

from docx.shared import Mm
from docxtpl import InlineImage, Listing, RichText, RichTextParagraph

_HERE = os.path.dirname(os.path.abspath(__file__))


def _img(name):
    return os.path.join(_HERE, os.pardir, "media", name)


def _sub(name):
    return os.path.join(_HERE, os.pardir, "templates", name)


def _media_path(name):
    """tests/fixtures/media 下素材的绝对路径（P7：replace_* 的路径入参）。"""
    return os.path.join(_HERE, os.pardir, "media", name)


def _media_bytes(name):
    """tests/fixtures/media 下素材的字节（P7：file-like 入参用 BytesIO 包）。"""
    with open(_media_path(name), "rb") as fh:
        return fh.read()

def build_context(tpl):
    title = RichText("INV-2026-001", bold=True, color="1F4E79", size="24")
    rows = [
        {"name": RichText("Alpha", bold=True), "qty": 1,
         "img": InlineImage(tpl, _img("p4_dot2x1.png"))},
        {"name": RichText("Beta", italic=True), "qty": 2,
         "img": InlineImage(tpl, _img("p4_dot2x1.png"))},
    ]
    return {"title": title, "rows": rows,
            "notes": Listing("first line\nsecond line\tlast")}

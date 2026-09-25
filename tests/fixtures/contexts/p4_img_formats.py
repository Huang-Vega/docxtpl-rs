# -*- coding: utf-8 -*-
"""P4 fixture context（由 tests/fixtures/generate.py 生成，勿手改）。

提供 build_context(tpl)：返回渲染上下文，可包含 RichText/RichTextParagraph/
Listing/InlineImage 等类型化值（对齐上游 docxtpl 0.20.2 用法）。
"""
import os

from docx.shared import Mm
from docxtpl import InlineImage, Listing, RichText, RichTextParagraph

_HERE = os.path.dirname(os.path.abspath(__file__))


def _img(name):
    return os.path.join(_HERE, os.pardir, "media", name)

def build_context(tpl):
    return {"bmp": InlineImage(tpl, _img("p4_brick4x2.bmp")),
            "gif": InlineImage(tpl, _img("p4_arrow4x2.gif")),
            "tif": InlineImage(tpl, _img("p4_tile4x2.tiff"))}

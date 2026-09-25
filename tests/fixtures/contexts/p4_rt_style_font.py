# -*- coding: utf-8 -*-
"""P4 fixture context（由 tests/fixtures/generate.py 生成，勿手改）。

提供 build_context(tpl)：返回渲染上下文，可包含 RichText/RichTextParagraph/
Listing/InlineImage/Subdoc 等类型化值（对齐上游 docxtpl 0.20.2 用法）。
"""
import os

from docx.shared import Mm
from docxtpl import InlineImage, Listing, RichText, RichTextParagraph

_HERE = os.path.dirname(os.path.abspath(__file__))


def _img(name):
    return os.path.join(_HERE, os.pardir, "media", name)


def _sub(name):
    return os.path.join(_HERE, os.pardir, "templates", name)

def build_context(tpl):
    rt1 = RichText("样式", style="Emphasis")
    rt1.add("区域字体", font="eastAsia:SimSun")
    rt2 = RichText("上标", superscript=True, lang="zh-CN")
    rt3 = RichText("RTL", bold=True, rtl=True)
    rt3.add("下标", subscript=True)
    return {"rt1": rt1, "rt2": rt2, "rt3": rt3}

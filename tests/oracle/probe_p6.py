# -*- coding: utf-8 -*-
"""P6 探针：验证 docxcompose Subdoc 构造期的字节级行为。

1. 主 styles.xml 在 deepcopy append 自定义样式后的声明形态
   （lxml 是否给副本元素带 xmlns 冗余声明）。
2. Subdoc._get_xml() 片段的精确形态（body 声明是否随剥标签丢失）。
3. Heading 1 / 自定义样式两侧 styles.xml 的 styleId/name 对照。
4. python-docx 默认模板的 parts 清单（确认 footnotes.xml 存在）。
"""

import copy
import io
import tempfile
from pathlib import Path

from docx import Document
from docx.enum.style import WD_STYLE_TYPE
from docxcompose.composer import Composer  # noqa: F401
from docxtpl import DocxTemplate
from docxtpl.subdoc import Subdoc, SubdocComposer

TMP = Path(tempfile.mkdtemp(prefix="p6probe_"))

# ---- 生成 sub.docx（与未来 fixture 同源：默认样式 + Heading 1 + 自定义样式） ----
sub = Document()
sub.add_paragraph("普通段落")
sub.add_heading("标题一", level=1)
custom = sub.styles.add_style("Custom Sub Style", WD_STYLE_TYPE.PARAGRAPH)
sub.add_paragraph("自定义样式段落", style=custom)
sub_path = TMP / "sub.docx"
sub.save(sub_path)

print("== sub styles ==")
for s in sub.styles:
    print(f"  id={s.style_id!r} name={s.name!r}")
print("== sub parts ==")
for rel in sub.part.rels.values():
    print(f"  {rel.rId} {rel.reltype.rsplit('/', 1)[-1]} external={rel.is_external}")
print("  footnotes part:", any("footnotes" in str(r.target_ref) for r in sub.part.rels.values() if not r.is_external))

# ---- 主模板：python-docx 默认 + {{p sd }} 段落 ----
main = Document()
main.add_paragraph("主模板前文")
main.add_paragraph("{{p sd }}")
main.add_paragraph("主模板后文")

# 手动走 attach_parts 的 add_styles append 分支，观察声明行为
main_path = TMP / "main.docx"
main.save(main_path)

tpl = Document(main_path)
composer = SubdocComposer(tpl)  # Subdoc 内部同款（attach_parts 在 docxtpl.SubdocComposer）
subdocx = Document(str(sub_path))
composer.attach_parts(subdocx)  # 直接调 Composer.attach_parts（Subdoc 内部同款）

styles_part = None
for rel in tpl.part.rels.values():
    if rel.reltype.rsplit("/", 1)[-1] == "styles":
        styles_part = rel.target_part
print("== main styles.xml after attach (raw bytes) ==")
xml = styles_part.blob.decode("utf-8")
# 只打印自定义样式附近，看声明形态
idx = xml.find("CustomSubStyle")
print(xml[max(0, idx - 200) : idx + 300])
print("...")
print("== main styles ids ==")
all_ids = [s.style_id for s in tpl.styles]
print(f"  共 {len(all_ids)} 个; CustomSubStyle in ids: {'CustomSubStyle' in all_ids}; Heading1 in ids: {'Heading1' in all_ids}")

# ---- Subdoc 片段形态 ----
tpl2 = DocxTemplate(str(main_path))
sd = Subdoc(tpl2, str(sub_path))
fragment = sd._get_xml()
print("== fragment (first 600 chars) ==")
print(fragment[:600])
print("== fragment has xmlns declarations? ==")
import re  # noqa: E402

decls = re.findall(r'xmlns[^ >]*="[^"]*"', fragment[:5000])
print("  ", decls[:10] if decls else "无（声明随剥标签丢失，依赖主根）")

# ---- 渲染端到端：真实 docxtpl 验证 {{p sd }} 写法 ----
from docxtpl import DocxTemplate  # noqa: E402

tpl3 = DocxTemplate(str(main_path))
sd3 = tpl3.new_subdoc(str(sub_path))
ctx = {"sd": sd3}
buf = io.BytesIO()
tpl3.render(ctx)
tpl3.save(buf)
print("== rendered OK, bytes =", len(buf.getvalue()))

# 渲染产物 document.xml 的片段落点
from docx import Document as D2  # noqa: E402

out = D2(buf)
print("== rendered document.xml (first 1200 chars) ==")
for rel in out.part.rels.values():
    if rel.reltype.rsplit("/", 1)[-1] == "document":
        print(rel.target_part.blob.decode("utf-8")[:1200])
print("== probe done ==")

# -*- coding: utf-8 -*-
"""P7 探针：实证 docxtpl 0.20.2 媒体替换族的字节级行为。

验证项：
1. add_picture 后 wp:docPr / pic:cNvPr 的 id/name/title/descr 值；
2. replace_pic 按 name/title/descr 匹配后的 media blob、partname、rels/CT 形态；
   missing 时 ValueError 文案；pic_map 内容；
3. replace_media 用 CRC32 换同扩展名/异扩展名内容后：partname/CT 是否不变、
   新 blob 是否落盘、extent 是否保留旧版式；
4. replace_media 作用于页眉 dummy 图；
5. replace_embedded / replace_zipname：手工挂可达 embeddings part 后，
   python-docx 保存是否保留该 part，CRC/zipname 替换是否命中；
6. 不 render 直接 save（纯替换）与渲染后 save 的 docPr id 差异；
7. header/footer part 被多条 rel 引用时 _replace_pics 的遍历与幂等。
"""

import binascii
import io
import os
import sys
import tempfile
import zipfile
from pathlib import Path

from docx import Document
from docx.opc.constants import RELATIONSHIP_TYPE as RT
from docx.opc.part import Part
from docx.opc.packuri import PackURI
from docx.oxml.ns import qn
from docxtpl import DocxTemplate

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "fixtures"))
import generate  # noqa: E402

TMP = Path(tempfile.mkdtemp(prefix="p7probe_"))
print("tmp:", TMP)


def zip_entries(path):
    with zipfile.ZipFile(path) as zf:
        return {i.filename: (i.CRC, i.file_size, i.date_time) for i in zf.infolist()}


def media_bytes(path, name="word/media"):
    with zipfile.ZipFile(path) as zf:
        out = {}
        for n in zf.namelist():
            if n.startswith(name + "/"):
                out[n] = zf.read(n)
        return out


def cnvpr_dump(blob_xml_part):
    """从 part blob 抽 graphicData/cNvPr/blip 信息。"""
    from lxml import etree

    root = etree.fromstring(blob_xml_part)
    ns = {
        "a": "http://schemas.openxmlformats.org/drawingml/2006/main",
        "pic": "http://schemas.openxmlformats.org/drawingml/2006/picture",
        "r": "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
        "wp": "http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing",
    }
    rows = []
    for gd in root.xpath("//a:graphic/a:graphicData", namespaces=ns):
        uri = gd.get("uri")
        cn = gd.xpath("pic:pic/pic:nvPicPr/pic:cNvPr", namespaces=ns)
        blips = gd.xpath("pic:pic/pic:blipFill/a:blip", namespaces=ns)
        rows.append({
            "uri": uri,
            "name": cn[0].get("name") if cn else None,
            "title": cn[0].get("title") if cn else None,
            "descr": cn[0].get("descr") if cn else None,
            "embed": blips[0].get(qn("r:embed")) if blips else None,
        })
    return rows


# ---------- 探针 1/2/3：正文图片 + replace_pic + replace_media ----------
dummy = generate.minimal_png()                 # 1x1 png
replacement = generate.make_png(3, 2)          # 3x2 png，字节不同
jpg_like = generate.make_png(4, 4)             # 仍是 png 字节，测异 CT 风险略过，先同 ext

tpl_path = TMP / "t1.docx"
doc = Document()
doc.add_paragraph("{{ x }}")
doc.add_picture(io.BytesIO(dummy), width=generate.Inches(1))
doc.save(tpl_path)

with zipfile.ZipFile(tpl_path) as zf:
    media_name = [n for n in zf.namelist() if n.startswith("word/media/")][0]
    orig_ct = zf.read("[Content_Types].xml").decode()
print("== 1. 模板 media 条目:", media_name, "crc=", hex(binascii.crc32(dummy) & 0xFFFFFFFF))
print("   CT 含 png default?", "image/png" in orig_ct)

tpl = DocxTemplate(tpl_path)
tpl.render({"x": "hello"})
# cNvPr 信息（渲染后 part blob）
rows = cnvpr_dump(tpl.docx.part.blob)
print("== 2. 渲染后正文 cNvPr:", rows)

out1 = TMP / "o1.docx"
tpl.save(out1)
base_entries = zip_entries(out1)
base_media = media_bytes(out1)
print("   保存后 media:", {k: len(v) for k, v in base_media.items()})

# replace_pic：用 name 匹配
pic_name = rows[0]["name"]
tpl2 = DocxTemplate(tpl_path)
tpl2.render({"x": "hello"})
tpl2.replace_pic(pic_name, io.BytesIO(replacement))
out2 = TMP / "o2.docx"
tpl2.save(out2)
m2 = media_bytes(out2)
print("== 3. replace_pic(name=%r) 后 media 命中替换:" % pic_name,
      {k: (len(v), v == replacement) for k, v in m2.items()})
print("   pic_map:", {k: v[0] for k, v in tpl2.get_pic_map().items()})

# replace_pic missing
tpl3 = DocxTemplate(tpl_path)
tpl3.render({"x": "hi"})
tpl3.replace_pic("不存在的图片.png", io.BytesIO(replacement))
try:
    tpl3.save(TMP / "o3.docx")
    print("== 4. missing pic 未报错（意外）")
except ValueError as e:
    print("== 4. missing pic ValueError:", e)

# replace_media：src 路径为原 dummy 字节，CRC 匹配
tpl4 = DocxTemplate(tpl_path)
tpl4.render({"x": "hi"})
src = TMP / "dummy.png"
src.write_bytes(dummy)
tpl4.replace_media(str(src), io.BytesIO(replacement))
out4 = TMP / "o4.docx"
tpl4.save(out4)
with zipfile.ZipFile(out4) as zf:
    m4_name = [n for n in zf.namelist() if n.startswith("word/media/")][0]
    m4 = zf.read(m4_name)
    ct4 = zf.read("[Content_Types].xml").decode()
    doc4 = zf.read("word/document.xml").decode()
print("== 5. replace_media 后 partname 不变?", m4_name == media_name,
      " blob=replacement?", m4 == replacement,
      " CT 不变?", ct4 == orig_ct)
import re as _re
extents = _re.findall(r'<wp:extent cx="(\d+)" cy="(\d+)"', doc4)
print("   替换后 extent（应保留 dummy 的 1 inch 版式）:", extents)

# ---------- 探针 6：页眉 dummy 图 replace_media ----------
from docx.enum.section import WD_HEADER_FOOTER  # noqa: E402

tpl5_path = TMP / "t5.docx"
d5 = Document()
sec = d5.sections[0]
hdr = sec.header
hp = hdr.paragraphs[0]
hp.add_run().add_picture(io.BytesIO(dummy), width=generate.Inches(0.5))
d5.add_paragraph("body")
d5.save(tpl5_path)
tpl5 = DocxTemplate(tpl5_path)
tpl5.render({})
tpl5.replace_media(str(src), io.BytesIO(replacement))
out5 = TMP / "o5.docx"
tpl5.save(out5)
with zipfile.ZipFile(out5) as zf:
    medias5 = {n: zf.read(n) for n in zf.namelist() if n.startswith("word/media/")}
print("== 6. 页眉替换 media:", {k: (len(v), v == replacement) for k, v in medias5.items()})

# ---------- 探针 7：手工挂 embeddings part 的可达性 ----------
tpl7_path = TMP / "t7.docx"
d7 = Document()
d7.add_paragraph("embed probe")
embed_uri = PackURI("/word/embeddings/probe_obj.bin")
embed_ct = "application/vnd.openxmlformats-officedocument.oleObject"
embed_part = Part(embed_uri, embed_ct, b"ORIGINAL-EMBED-BYTES-XYZ", d7.part.package)
d7.part.relate_to(
    embed_part,
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/oleObject",
)
d7.save(tpl7_path)
with zipfile.ZipFile(tpl7_path) as zf:
    names7 = zf.namelist()
    ct7 = zf.read("[Content_Types].xml").decode()
print("== 7. embeddings part 保存后存活?", "word/embeddings/probe_obj.bin" in names7)
print("   CT override 存活?", "probe_obj.bin" in ct7)
orig_embed = b"ORIGINAL-EMBED-BYTES-XYZ"
print("   crc=", hex(binascii.crc32(orig_embed) & 0xFFFFFFFF))

tpl7 = DocxTemplate(tpl7_path)
tpl7.render({})
src_embed = TMP / "orig.bin"
src_embed.write_bytes(orig_embed)
tpl7.replace_embedded(str(src_embed), TMP / "new.bin") if (TMP / "new.bin").exists() else None
(TMP / "new.bin").write_bytes(b"NEW-EMBED-BYTES-000000")
tpl7.replace_embedded(str(src_embed), str(TMP / "new.bin"))
out7 = TMP / "o7.docx"
tpl7.save(out7)
with zipfile.ZipFile(out7) as zf:
    got7 = zf.read("word/embeddings/probe_obj.bin")
print("   replace_embedded 命中?", got7 == b"NEW-EMBED-BYTES-000000")

# replace_zipname 单独测（post_processing 中 zipname 判定优先于 CRC）
tpl7z = DocxTemplate(tpl7_path)
tpl7z.render({})
(TMP / "new_zip.bin").write_bytes(b"NEW-ZIPNAME-BYTES")
tpl7z.replace_zipname("word/embeddings/probe_obj.bin", str(TMP / "new_zip.bin"))
out7z = TMP / "o7z.docx"
tpl7z.save(out7z)
with zipfile.ZipFile(out7z) as zf:
    got7z = zf.read("word/embeddings/probe_obj.bin")
print("   replace_zipname 命中?", got7z == b"NEW-ZIPNAME-BYTES")

# ---------- 探针 8：不 render 直接 save（纯替换） ----------
tpl8 = DocxTemplate(tpl_path)
tpl8.replace_media(str(src), io.BytesIO(replacement))
out8 = TMP / "o8.docx"
tpl8.save(out8)
with zipfile.ZipFile(out8) as zf:
    doc8 = zf.read("word/document.xml").decode()
ids8 = _re.findall(r'<wp:docPr id="(\d+)"', doc8)
print("== 8. 纯替换不渲染 docPr id:", ids8, "（渲染路径应为 1001）")

# ---------- 探针 9：get_undeclared_template_variables ----------
t9_path = TMP / "t9.docx"
d9 = Document()
d9.add_paragraph("{{ a }} {% if b %}{{ c }}{% endif %}")
d9.add_paragraph("{%p for item in items %}")
d9.add_paragraph("{{ item.name }}")
d9.add_paragraph("{%p endfor %}")
d9.save(t9_path)
t9 = DocxTemplate(t9_path)
v9 = t9.get_undeclared_template_variables()
v9b = t9.get_undeclared_template_variables(context={"a": 1, "items": []})
print("== 9. undeclared:", sorted(v9), "after ctx:", sorted(v9b))

print("DONE")

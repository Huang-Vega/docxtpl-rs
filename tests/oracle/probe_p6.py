# -*- coding: utf-8 -*-
"""P6 probe: verify byte-level behavior at docxcompose Subdoc construction.

1. Declaration shape of the main styles.xml after deepcopy-appending a custom
   style (whether lxml adds redundant xmlns declarations to the copied
   element).
2. Exact shape of the Subdoc._get_xml() fragment (whether body declarations
   are lost when tags are stripped).
3. styleId/name comparison of styles.xml on both sides for Heading 1 / the
   custom style.
4. Parts list of the python-docx default template (confirm footnotes.xml
   exists).
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

# ---- build sub.docx (same provenance as the future fixture: default styles + Heading 1 + custom style) ----
sub = Document()
sub.add_paragraph("plain paragraph")
sub.add_heading("Heading One", level=1)
custom = sub.styles.add_style("Custom Sub Style", WD_STYLE_TYPE.PARAGRAPH)
sub.add_paragraph("custom style paragraph", style=custom)
sub_path = TMP / "sub.docx"
sub.save(sub_path)

print("== sub styles ==")
for s in sub.styles:
    print(f"  id={s.style_id!r} name={s.name!r}")
print("== sub parts ==")
for rel in sub.part.rels.values():
    print(f"  {rel.rId} {rel.reltype.rsplit('/', 1)[-1]} external={rel.is_external}")
print("  footnotes part:", any("footnotes" in str(r.target_ref) for r in sub.part.rels.values() if not r.is_external))

# ---- main template: python-docx default + {{p sd }} paragraph ----
main = Document()
main.add_paragraph("main template before")
main.add_paragraph("{{p sd }}")
main.add_paragraph("main template after")

# Manually exercise the add_styles append branch of attach_parts to observe
# its declaration behavior
main_path = TMP / "main.docx"
main.save(main_path)

tpl = Document(main_path)
composer = SubdocComposer(tpl)  # same one used inside Subdoc (attach_parts lives in docxtpl.SubdocComposer)
subdocx = Document(str(sub_path))
composer.attach_parts(subdocx)  # call Composer.attach_parts directly (same one used inside Subdoc)

styles_part = None
for rel in tpl.part.rels.values():
    if rel.reltype.rsplit("/", 1)[-1] == "styles":
        styles_part = rel.target_part
print("== main styles.xml after attach (raw bytes) ==")
xml = styles_part.blob.decode("utf-8")
# print only the vicinity of the custom style to inspect declaration shape
idx = xml.find("CustomSubStyle")
print(xml[max(0, idx - 200) : idx + 300])
print("...")
print("== main styles ids ==")
all_ids = [s.style_id for s in tpl.styles]
print(f"  {len(all_ids)} in total; CustomSubStyle in ids: {'CustomSubStyle' in all_ids}; Heading1 in ids: {'Heading1' in all_ids}")

# ---- Subdoc fragment shape ----
tpl2 = DocxTemplate(str(main_path))
sd = Subdoc(tpl2, str(sub_path))
fragment = sd._get_xml()
print("== fragment (first 600 chars) ==")
print(fragment[:600])
print("== fragment has xmlns declarations? ==")
import re  # noqa: E402

decls = re.findall(r'xmlns[^ >]*="[^"]*"', fragment[:5000])
print("  ", decls[:10] if decls else "none (declarations lost during tag stripping; relies on the main root)")

# ---- end-to-end render: verify the {{p sd }} form with real docxtpl ----
from docxtpl import DocxTemplate  # noqa: E402

tpl3 = DocxTemplate(str(main_path))
sd3 = tpl3.new_subdoc(str(sub_path))
ctx = {"sd": sd3}
buf = io.BytesIO()
tpl3.render(ctx)
tpl3.save(buf)
print("== rendered OK, bytes =", len(buf.getvalue()))

# where the fragment lands in the rendered document.xml
from docx import Document as D2  # noqa: E402

out = D2(buf)
print("== rendered document.xml (first 1200 chars) ==")
for rel in out.part.rels.values():
    if rel.reltype.rsplit("/", 1)[-1] == "document":
        print(rel.target_part.blob.decode("utf-8")[:1200])
print("== probe done ==")

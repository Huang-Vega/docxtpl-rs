# ADR-009：P7b 真实 Word 模板字节对齐（patch 输入树往返 / 包级保存归一 / 脚注原字节往返 / 跨树命名空间归并）

日期：2026-09-26
状态：已接受
阶段：P7b（docxtpl 0.20.2 上游真实模板语料，16 个新 render fixture）

## 背景

P0–P7a 的 85 个 render fixture 均为程序化合成模板：其 XML 字节本就
经过 lxml 规范化（单引号声明、LF、无元素间缩进），与上游渲染管线的
中间形态同构。docxtpl 0.20.2 仓库 `tests/templates/` 下的 16 个真实
Word 模板（Word 2016 保存形态：双引号声明 + CRLF、元素间缩进、段落
局部命名空间、rels 登 Override、customXml 部件等）跑出差分后暴露
6 个阻断差异族（探针日志 `.trae/p7b_probe/`）：

- **B1**：保存期包级归一差异——`[Content_Types].xml` 的 rels
  Override 与 Default 归属、全部 rels 的声明词法、
  styles/settings/numbering 等已知 XmlPart 的声明/缩进形态；
- **B2**：正文/页眉页脚喂给 `patch_xml` 的输入不是磁盘原字节，而是
  python-docx oxml 树序列化结果（实体解码、缩进剥除）；
- **B3**：`{_% %_}` 字面转义还原把 `{_%` 错还原成 `{%_`（一行错）；
- **B4**：真实模板段落带局部 `xmlns:wp14`（与根 `xmlns:w14` 同
  URI 不同前缀），上游输出统一为祖先前缀 `w14` 并丢弃局部声明；
- **B5**：无标签脚注 part 的 Word 原始声明/CRLF 形态被本侧树往返
  破坏；
- **B6**：疑似 eastAsia 字体、空格/Tab RichText 不兼容（探针后证实
  已天然支持，**零改动**）。

本 ADR 记录 B1–B5 的移植决策；语料为 docxtpl 0.20.2 的 LGPL-2.1
测试模板，复制入 `tests/fixtures/sources/p7b_*.docx`。

## 探针钉死的上游事实（docxtpl 0.20.2 + python-docx 1.2.0 + lxml 6.1.1）

1. **patch_xml 输入三路径**（template.py L289-461）：正文
   `etree.tostring(body)`；页眉/页脚
   `etree.tostring(parse_xml(part.blob))`；脚注 `part.blob.decode()`。
   前两者出自 python-docx oxml 解析树（`opc/oxml.py` L21：
   `remove_blank_text=True, resolve_entities=False`）——元素间缩进
   空白剥除、属性实体解码为字面字符（jinja 表达式里的
   `&quot;`/`&apos;` 不再残留）、空元素词法归一。
2. **`serialize_part_xml`**（oxml.py）=
   `etree.tostring(elm, encoding="UTF-8", standalone=True)` →
   `<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n`
   （**单引号**，声明后恒一个 LF；无 XML 内容时无尾行）。
3. **B3**：上游 template.py L323-328 的字面还原是
   `xml = xml.replace("{_%", "{%")`（本侧误写成 `{%_`）。
4. **B1（a）CT 重建**：`PackageWriter._write_content_types_stream`
   → `_ContentTypesItem.from_parts`（pkgwriter.py L80-99）每次保存
   无条件预置 `Default rels`/`Default xml`，遍历 parts 调
   `_add_content_type`：扩展名命中 python-docx
   `docx/opc/spec.py` `default_content_types` 表落 Default、其余落
   Override；rels 部件不在 parts 枚举。真实 Word 模板里
   `/_rels/.rels`、`/word/_rels/*.rels` 的 rels Override 与
   `customXml/item1.xml` 的 application/xml Override 保存后消失
   （hf_entities 期望 CT 实证：开头两个 Default rels/xml、仅 13
   个 Override）。`to_xml` 时 Default 按 ext、Override 按 partname
   ASCII 排序。
5. **B1（b）rels 恒重写**：根 rels 与全部挂接 rels（含
   `customXml/_rels/item1.xml.rels`）由 PackageWriter 以模型
   `Relationships` 重写为 lxml 形态；关系内容相同、仅声明词法
   （双引号+CRLF、缺 standalone、尾部换行）差异。
6. **B1（c）已知 XmlPart 恒重序列化**：python-docx PartFactory
   注册的 XmlPart 子类（document/header/footer/core **/styles/
   settings/numbering**）保存时 `XmlPart.blob` =
   `serialize_part_xml(element)`（part.py L221），即使渲染未触碰
   也重写；order 模板实证 settings.xml/styles.xml 内容一致、仅
   声明（双引号 CRLF vs 单引号 LF）差异。其余 part
   （fontTable/webSettings/theme/footnotes/endnotes/comments/
   customXml 等）是通用 Part，`blob` 返回加载原字节。
7. **B5 脚注是通用 Part**：footnotes+xml 未在 PartFactory 注册，
   `render_footnotes` 直接对 `part.blob.decode()` 做 patch/jinja 后
   写回 `part._blob`——模板的 Word 双引号声明原样穿过；comments/
   footnotes_real 期望实证声明为 `version="1.0" ...`（双引号）。
8. **Jinja2 词法器吃 CR**：lexer `tnewline = \r\n|\r(?!\n)|\n`
   统一产出 NEWLINE token，渲染输出恒 LF——脚注原字节的声明后
   CRLF 经一次 jinja 往返后变 LF（长度 3452→3451 实证）。lxml 树
   输出本就无 CR，该规范化在正文/story 路径不可观测。
9. **B4 跨树换挂归并命名空间**：正文渲染把
   `parse_xml(rendered)` 的 body 子节点**逐个 append** 进原 document
   树（`replace_children`）。lxml/libxml2 跨树移动元素时，若元素
   nsDef 的 URI 在目标树祖先轴已绑定（**任意前缀**），该 nsDef
   丢弃，元素名与全部后代元素/属性的前缀重绑到祖先前缀：
   `<w:p xmlns:wp14="U" w14:a="1" wp14:b="2"><w:c wp14:z="3"/></w:p>`
   换挂到 `xmlns:w14="U"` 的 document 下 →
   `<w:p w14:a="1" w14:b="2"><w:c w14:z="3"/></w:c>`。纯
   parse/tostring **不触发**归并（探针实证冗余声明原样保留）——
   故整树解析输出的页眉/页脚路径保持词法。

## 决策

1. **fixture 机制**（tests/fixtures/generate.py）：`@fixture` 新增
   `source`/`source_upstream` 参数；source 非 None 时从
   `tests/fixtures/sources/` 复制 docx、不程序化 build，manifest 落
   `"source": "docxtpl-0.20.2:tests/templates/<原名>"`。新增 16 个
   `p7b_*`（编号 106-121），其中 word2016/cellbg/richtext_if/
   eastasia 为 `context_kind="python"`，oracle_diff.rs 侧 1:1 复刻
   4 个 Rust arms（RichText::text/text_with + props()，cellbg 用
   RenderValue::array/object 构造）。LGPL-2.1 语料随仓库存放。
2. **B2/B1（c）共用 `normalize_part_xml(src, part_name)`**
   （docxtpl-template 公开导出）：`parse_strict` →
   `strip_blank_text()` → 恒输出单引号声明 + LF +
   `serialize_subtree(root)`。正文/story 的 patch 前输入与保存期
   styles/settings/numbering 无条件归一共用同一形态；良构模板解析
   失败按 XML 错误（part 名）上报，与上游打开文档即失败一致。
   预处理保留声明是必要的：剥掉声明会使渲染后 lenient 重解析的树
   丢失 `has_decl`，最终输出缺声明行（曾造成 96 例存量回归）。
3. **B5 脚注开关**：`render_part_string` 增加
   `normalize_input: bool`——正文/story 传 true；脚注传 false
   （原字节直入 patch，保留模板声明词法）。同一入口统一做
   `.replace("\r\n","\n").replace('\r',"\n")`，对齐 Jinja2 lexer
   的 tnewline 规范化（仅脚注路径可观测）。
4. **B1（a）opc CT 重建**：docxtpl-opc 新增
   `DEFAULT_CONTENT_TYPES`（1:1 移植 spec.py default_content_types：
   bin×3 printerSettings、bmp/emf/fntdata/gif/jpe/jpeg/jpg/png/rels/
   tif/tiff/wdp/wmf/xlsx/xml）、`OPC_RELATIONSHIPS_CT`/`XML_CT`
   常量与 `ContentTypes::rebuild_from_parts(parts)`（预置 rels/xml
   Default，其余按小写扩展名命中表落 Default 否则 Override，整体
   替换；排序仍在 `to_xml`）。
5. **B1（b/c）Package 归一 API**：`Package::rebuild_content_types`
   枚举除目录/CT/`.rels` 外全部 part 按当前 CT 重建视图；
   `Package::normalize_relationships` 重写根 rels 与全部挂接 rels
   为 `Relationships::to_xml()` 规范字节。docxtpl-rs 保存前
   `canonicalize_content_types` 升级为三步：
   `normalize_known_xml_parts`（styles/settings/numbering **白名单
   CT** 树往返，字节未变不写；document/hdr/ftr/core 已在渲染管线
   重写，通用 Part 继续透传）→ `normalize_relationships` →
   `rebuild_content_types` + CT 差异写回。render() 与
   RenderSession::finish 共用该入口。
6. **B4 序列化期换挂重绑**（docxtpl-xml serialize.rs）：
   `retain_redundant_ns=false`（正文/Subdoc 片段）路径新增
   `ancestor_uri_prefix(node, uri)`——沿祖先轴（由近及远）查 URI
   的**有效**绑定前缀（递归处理内层同前缀不同 URI 遮蔽）：
   元素 nsDecl 与祖先任意前缀同 URI 时省略该声明；元素名与属性名
   前缀按 URI 重绑到祖先前缀。`retain_redundant_ns=true`
   （页眉/页脚整树 parse/tostring + 注入片段）保持全部词法
   （ADR-006）。旧的同前缀专用查询 `ancestor_ns_binding` 删除。
7. **B3 一行修**（docxtpl-template render.rs）：
   `.replace("{%_", "{%")` → `.replace("{_%", "{%")`。
8. **B6 零改动**：eastAsia 字体前缀、纯空格/Tab RichText 经 4 个
   python-context fixture 逐字节验证天然支持。

## 影响

- 新公开 API：`docxtpl_template::normalize_part_xml`；
  docxtpl-opc `ContentTypes::rebuild_from_parts` 与
  `Package::rebuild_content_types`/`normalize_relationships`
  （默认内容类型表为 crate 私有常量）。
- 无新第三方依赖（spec.py 默认表为常量移植）。
- 验收：**101 个 render fixture 全部与 Python oracle 一致**
  （97 逐 part 字节/c14n 双匹配 + 4 错误类别：Syntax×2、Image×1、
  Value×1），16 个 p7b 全 MATCH；golden 计数 101（full_patched）/
  99（pre_recover，101 中 r2_syntax_error/p4_img_bad 仅导出
  patched）；85 存量零回归。
- 本阶段不新增 DEV：通用 Part blob 透传、ns 重绑等均为对齐上游
  行为；既存排除项（DEV-0005 autoescape+富值、DEV-0008 Subdoc 仅
  docpath、自定义 jinja_env 拒绝清单）继续适用。
- P7b 实施时本机无 Office/LibreOffice 实机抽查（语料源自上游仓库，字节
  差分已逐 part 钉死）；P7d 后续已补齐 LibreOffice 8/8 与 Microsoft Word
  10/10 代表性实机抽查，详见 `docs/p7-compatibility-report.md`。

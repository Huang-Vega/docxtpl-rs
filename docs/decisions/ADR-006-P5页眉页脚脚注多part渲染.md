# ADR-006：P5 页眉/页脚/脚注多 part 渲染

日期：2026-09-25
状态：已接受
阶段：P5（页眉、页脚、脚注）

## 背景

P0–P4 的渲染只改写 `word/document.xml`，真实文档的可变内容还会出现在
header/footer story part（`word/headerN.xml`、`word/footerN.xml`）、
核心属性（`docProps/core.xml`，P3 已支持）与脚注 part
（`word/footnotes.xml`）。上游 `DocxTemplate.render` 对这些 part 各有
不同的处理路径，且 InlineImage 的关系/媒体副作用在多 part 下必须按
作用域隔离。本 ADR 记录 P5 的 part 编排、序列化差异与多 owner 模型。

## 探针钉死的上游事实（docxtpl 0.20.2 + python-docx 1.2.0 + lxml 6.1.1）

1. **固定渲染顺序**（`template.py` render 主体）：body
   （`fix_tables` + `fix_docpr_ids`）→ headers（主文档 rels 中的 header
   关系序）→ footers（footer 关系序）→ `render_properties` →
   `render_footnotes`。
2. **header/footer 经 `get_part_xml` + `render_xml_part` 渲染**：
   - `get_part_xml` 先 `parse_xml(part.blob)`（python-docx oxml 解析器，
     `remove_blank_text=True`）再 `etree.tostring(unicode)`；随后与正文
     共用 `patch_xml`、`\n<w:p` 插行、jinja、还原、`resolve_listing`；
     **不做** `fix_tables` / `fix_docpr_ids`。
   - 落盘时 `map_headers_footers_xml` 用 `XmlPart.load` 以渲染字符串
     **重新建 part**（独立解析的新树，不换挂到任何原始元素），最终
     `etree.tostring(el, encoding="UTF-8", standalone=True)` →
     单引号声明 + 换行；新 part 复制原 part 的全部 rels。
3. **story part 的两处序列化差异**（oracle 差分实证）：
   - `remove_blank_text` 剥除元素间纯空白文本（注入图片 XML 里
     python-docx 模板自带的换行/缩进消失，图片片段变紧凑），但
     `xml:space="preserve"` 作用域内的空白文本保留（libxml2 沿祖先轴
     追踪该属性，`preserve` 保留、`default` 恢复裁剪）。
   - 注入的 `wp:inline` 自带 `wp/a/pic/r` 四个 xmlns 声明；正文因
     `map_tree` 把渲染树**换挂**到原始 document 元素下，lxml 序列化时
     剥掉与祖先重复的 `xmlns:wp/xmlns:r`（只剩 a/pic，且空白保留为
     多行缩进）；story 无换挂过程，四个冗余声明原样保留。
4. **脚注是通用二进制 Part**：footnotes content type 未在 python-docx
   PartFactory 注册，`part.blob.decode()` → patch → `render_xml_part`
   → `part._blob = xml.encode()`；模板的 XML 声明与未改字节原样保留，
   不做 XML 重解析/重序列化。通用 Part 上解析 InlineImage 会触发
   `AttributeError`（上游不支持脚注图片，记为 DEV-0006）。
5. **shape_id 为 part 级常数（修正 ADR-005 的早期表述）**：
   python-docx 1.2.0 的 `StoryPart.next_id` 是无缓存 `@property`，每次
   调用都对**原始 part 树** `xpath("//@id")` 取 max+1；渲染只产出字符串
   不回写 part 元素，因此同一 part 内所有新图的 docPr id/name 相同
   （正文再由 `fix_docpr_ids` 把 id 重排为 1001 起，name 不动；story
   与脚注不重编号）。
6. **图片关系按当前渲染 part 作用域分配**：`current_rendering_part`
   切到对应 XmlPart，InlineImage 的 blip/hyperlink 关系与新建 rels
   全部落在该 part（无 rels 则新建）；媒体 part 与 `media/imageN.ext`
   编号仍是包级 sha1 去重共享。`build_url_id` 的外链恒归主文档 rels。
7. **[Content_Types].xml 每次保存都重建**（`PackageWriter`）：
   从全部 part 重新汇总，Default 按扩展名、Override 按 part 名 ASCII
   排序；即使本次渲染没有新增图片，乱序 Override（如工具插入的
   footnotes Override）也会在保存时被归一。

## 决策

1. **渲染管线三模式**（docxtpl-template）：
   - `render_document_xml_ctx`（Document）：完整管线 + `fix_tables` +
     `fix_docpr_ids`，常规序列化（冗余 ns 裁剪、保留空白文本）。
   - `render_story_xml_ctx`（Story）：完整 patch/jinja/listing 管线但
     无 fix；序列化前 `strip_blank_text`（尊重 `xml:space`），输出走
     新的 `serialize_story`（保留元素词法自带的冗余 xmlns 声明）。
   - `render_footnotes_xml_ctx`（Footnotes）：只跑字符串阶段并原样
     返回（保留模板声明），内部使用 `NullRegistry`，出现 InlineImage
     即报错。
2. **part 编排集中在门面 `render_all_parts`**（docxtpl-rs）：
   body → story parts → core properties → footnotes，顺序与上游一致；
   story 目标经主文档 rels 枚举（两遍：先 `/header` 再 `/footer`），
   只收 Internal、相对 Target 按主文档 part **所在目录**
   （`PartUri::parent()`，不是 part 路径本身）解析后命中的非空 part，
   同目标去重；footnotes 经包 parts 的 content type 过滤枚举。
3. **多 owner 图片注册表**（`ImageInjections`）：owner 状态
   （name/rels_name/rels/rels_existed/dirty）入栈登记，
   `begin_owner` 幂等切换；新 rels part 先 `add_part` 挂载再
   `set_part_bytes`；`apply` 统一落定 pending media 与各脏 owner rels。
   sha1/media 编号保持包级共享；`build_url_id` 强制 current=主文档。
4. **图片惰性占位符解析**：`context_to_minijinja` 把 InlineImage
   编码成控制字符占位符，jinja 渲染后用正则按输出中的出现顺序解析
   （未被引用的图片不产生关系；同一图片多次出现各解析一次，rId 复用、
   docPr 共享同一常数 shape_id），任何一张图解析失败则整 part 返回
   带 part 名的错误。
5. **docxtpl-xml 新增两个能力**：`XmlDocument::strip_blank_text`
   （按 xml:space 作用域删除纯空白文本节点）与
   `serialize_story`（带 `retain_redundant_ns` 选项的序列化）；仅
   Story 模式使用，正文/脚注路径字节行为不变。
6. **每次渲染后归一 [Content_Types].xml**：`canonicalize_content_types`
   以现有表 `to_xml()` 重建（Default/Override 排序），字节未变则不
   写回（dirty 门控）。
7. **fixture**：新增 7 个 P5 fixture（hf_basic、hf_multi、
   footnotes_basic、hf_image、hf_richtext、hf_syntax_error(error 预期)、
   hf_untagged）；oracle 差分对 python 上下文（脚注富值、多图含 anchor、
   RichText+build_url_id）在测试内 1:1 复刻。不允许改 fixture/golden
   放宽断言。

## 影响

- 公开 API 无新增用户接口（多 part 渲染自动发生）；
  `render`/`render_ctx`/`RenderSession::finish` 行为统一。
- 错误信息现在带具体 story/脚注 part 名（如
  `part: "word/header1.xml"`）。
- 已知限制（compatibility.md §5）：DEV-0006 脚注不支持 InlineImage；
  DEV-0007 同一 part 双 header/footer 关系只渲染一次、endnotes 不在
  范围。
- 验收：73 个 render fixture 全部与 Python oracle 逐 part
  字节/c14n 一致（70 成功匹配 + 3 错误类别匹配），golden 计数
  73（patch）/71（recover）。

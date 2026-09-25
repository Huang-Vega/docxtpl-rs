# Changelog

所有用户可见变化均记录于此（代码规范 §10）。语义对齐基线：Python docxtpl 0.20.2。

## [0.1.0-alpha]

首个 MVP：P0–P3 范围全部完成，与 Python docxtpl 0.20.2 oracle 差分全绿
（49 个渲染 fixture：48 个逐字节 MATCH + 1 个错误类别一致；另含 20 个真实 docx
OPC 字节往返、49 个 patch golden、48 个 recover golden，详见
docs/compatibility.md §7）。

- docxtpl-opc：ZIP/OPC 包读写、`[Content_Types].xml` 与 relationship 索引、
  主文档定位、包校验、输入限额（条目数/大小/压缩比/输出上限）、未修改 part
  原始字节保留。
- docxtpl-xml：严格与宽松（libxml2 recover 风格）解析、Vec arena 可编辑树、
  逐字节对齐 lxml 的保留式序列化（单引号 XML 声明等）。
- docxtpl-compat：`patch_xml` 13 步正则变换与 `resolve_listing`（`\n \t \u07 \u0c`
  → br/tab/换段/分页）的逐条移植。
- docxtpl-template：正文渲染主管线（patch → 插/撤换行 → MiniJinja（lenient
  undefined，默认 autoescape 关）→ 字面转义还原 → resolve_listing → recover
  → fix_tables → fix_docpr_ids → lxml 风格序列化）；结构化标签 p/tr/tc/r、
  colspan/cellbg/vm/hm、表格网格增删修正、wp:docPr 自 1001 重编号（无前缀
  id 属性）。
- render_properties：随每次 render 无条件渲染 6 个字符串型核心属性
  （author/comments/identifier/language/subject/title），缺省 dc:identifier、
  dc:language 元素按上游 setattr 副作用补齐。
- docxtpl-rs：门面 API `DocxTemplate::open/from_reader/from_bytes`、
  `render(&json, &RenderOptions)`、`RenderedDocument::save/write_to/to_bytes`；
  稳定错误分类 `TemplateErrorKind::{Syntax,Undefined,Other}` 与对齐上游的
  docx_context 行上下文。
- docxtpl-cli：`docxtpl render <模板> <context.json> <输出> [--autoescape]`。
- 安全：禁 DTD/外部实体；包与 XML 深度/大小限额（ADR-004）。

## [Unreleased]

- **P4：RichText / RichTextParagraph / Listing / InlineImage（ADR-005）**。
  新增 17 个 `p4_*` oracle fixture，差分 16 个逐字节 MATCH + 1 个错误类别
  一致（UnrecognizedImageError），详见 docs/compatibility.md §4/§7。
  - 新 crate `docxtpl-rich`：富文本 run（全属性、`html.escape` 五字符
    转义、空串 falsy 语义）、富文本段落、Listing 转义文本；png/jpeg/gif/
    bmp/tiff 图片头解析（sha1、像素、dpi、扩展名/content-type）与 EMU 换算
    （单边缩放银行家舍入）；`wp:inline` XML 与上游 pretty 输出逐字符一致。
  - docxtpl-template：渲染管线泛化为类型化 `RenderContext`/`RenderValue` +
    `ImageRegistry` trait；渲染前对原始 document 计算共享 shape_id；
    新增 `TemplateErrorKind::Image`（oracle 异常 UnrecognizedImageError）。
  - docxtpl-opc：document rels 与 `[Content_Types].xml` 的 python-docx 风格
    重建 API（保序尾插、rId/编号空洞回填、Default/Override 排序仅发生在
    序列化）；新增 part 追加（Deflate）、part 目标相对路径解析。
  - docxtpl-rs：新门面 API `render_ctx(&RenderContext, ..)` 与一次性
    `render_session()`/`RenderSession::build_url_id(url)`（对齐上游
    `tpl.build_url_id`）；图片注入实现全包 DFS 基线收集、sha1 去重、
    `word/media/imageN.ext` 跨扩展名编号、图片 rId 先于锚点外链；
    media/rels/CT 变更渲染后一次性落定，未涉及 part 原字节保留。
- 收紧默认 OPC 限额，并在门面增加 128 MiB 压缩输入、64 MiB 渲染 XML
  及 MiniJinja 10 000 000 fuel 限额；超限返回明确错误。
- 锁定与 Rust 1.85 兼容的 `time`、`deflate64`；固定 golden XML 的换行字节，
  修复 Windows 测试受 Git 换行转换影响的问题。
- 新增 LibreOffice 打开并另存的 CI 检查、MVP 使用指南和 P1 限额校准记录。
- **P5：页眉/页脚/脚注多 part 渲染（ADR-006）**。新增 7 个 `p5_*` oracle
  fixture，差分 6 个逐字节 MATCH + 1 个错误类别一致（页眉
  TemplateSyntaxError，错误带 `word/header1.xml` part 名），详见
  docs/compatibility.md §4/§7。
  - docxtpl-template：渲染管线按 part 种类参数化为
    Document（fix_tables/fix_docpr_ids）/ Story（无 fix，story 专用
    序列化：剥除非 preserve 作用域空白文本、保留注入图片的冗余
    `xmlns:wp/xmlns:r` 声明）/ Footnotes（仅字符串阶段，保留模板声明，
    `NullRegistry` 拒绝脚注图片 = DEV-0006）；图片改为占位符惰性解析，
    shape_id 修正为 part 级常数（对齐 python-docx 1.2.0 无缓存的
    `StoryPart.next_id`，正文 docPr 仍由 fix_docpr_ids 重排为 1001 起）。
  - docxtpl-xml：新增 `XmlDocument::strip_blank_text`（沿祖先轴尊重
    `xml:space="preserve"`）与带 `retain_redundant_ns` 选项的
    `serialize_story`。
  - docxtpl-rs：`render_all_parts` 固定编排 正文 → 页眉 → 页脚 →
    核心属性 → 脚注；story 经主文档 rels（按 part 所在目录解析 Internal
    目标、非空、去重）枚举，footnotes 按 content type 枚举（DEV-0007：
    同目标双 rel 只渲染一次、endnotes/外部 story 不支持）；
    `ImageInjections` 泛化为多 owner 作用域（每 story 各自 rels，
    新建 rels 先挂载再写回；sha1/media 编号包级共享；`build_url_id`
    外链恒归主文档）；每次渲染后归一 `[Content_Types].xml`
    （Default/Override ASCII 排序，字节未变不写回）。

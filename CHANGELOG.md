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

- 收紧默认 OPC 限额，并在门面增加 128 MiB 压缩输入、64 MiB 渲染 XML
  及 MiniJinja 10 000 000 fuel 限额；超限返回明确错误。
- 锁定与 Rust 1.85 兼容的 `time`、`deflate64`；固定 golden XML 的换行字节，
  修复 Windows 测试受 Git 换行转换影响的问题。
- 新增 LibreOffice 打开并另存的 CI 检查、MVP 使用指南和 P1 限额校准记录。

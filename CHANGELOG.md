# Changelog

所有用户可见变化均记录于此（代码规范 §10）。语义对齐基线：Python docxtpl 0.20.2。

## [Unreleased]

- 修复严格 XML 解析器对畸形 `<!` 声明中多字节 Unicode 字符进行前缀切片时的 panic；新增定向回归与持久化属性测试 seed。
- 补齐 macOS 26.6.2 arm64 的质量门禁、oracle、MSRV、Office 与性能实测证据。

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

- **P7c/P7d：兼容加固、审计与 0.8.0 冻结（ADR-010）**。
  - 新增 OPC 随机/截断字节、XML strict/recover 任意输入和 marker-heavy
    `patch_xml` 共 6 个 proptest 性质（每项 256/512 cases），错误路径无 panic。
  - 对 127 个模板复核资源峰值；默认限额继续覆盖最大 23 条目、833 014 B
    总解压和 32.156 压缩比，未发现高危包校验缺陷。
  - 新增 `tests/p7c_audit.py` 与 Windows release 三样例分阶段性能基线，拆分
    open/render/write、排除 CLI 启动并记录峰值 RSS；同平台 render 回退超过
    20% 可阻断；CI 保持 Windows/Linux/macOS 回归，
    clippy 扩至 `--all-features`。
  - 冻结 P7 兼容报告与公开 DEV 边界，workspace 版本提升为 0.8.0；
    不包含 crates.io 发布或 Git 标签。
  - 后续审计补齐 `DocxTemplate::picture_map()`（上游 `get_pic_map`）及
    reset/zipname 优先级回归，并将三平台、Office、持续 fuzz、峰值内存
    明确保留为发布前证据，不再把 CI 配置等同于实测结果。
  - 新增 `p4_autoescape_rich` oracle，修复 autoescape 下富值被转义的
    DEV-0005；基线升至 102 render（98 MATCH + 4 错误类别），golden
    102/100。增加重复/外部 story、SmartArt/VML/脚注拒绝和 P7 descr/
    未命中/注册顺序测试；Subdoc 原始字符串改为严格校验的 opaque 片段。
  - 增加四个 `cargo-fuzz` target（OPC、strict/recover XML、patch_xml、完整
    from_bytes/render）及每周有界 fuzz 工作流；补充页眉图片与核心属性语法
    错误的 part 定位集成回归，并清理 224 个仅 ZIP 时间戳变化的 DOCX。
  - 峰值 RSS 采集扩展到 Windows/Linux/macOS，性能比较显式拒绝跨 OS/架构
    基线，避免把机器差异误报为性能回退。
  - 补齐 DEV-0008 路径模式/借用模式 API 边界和 DEV-0010 缺 numbering/
    编号重启拒绝分支；核心属性 part 缺失时按 python-docx 重建，非 UTF-8
    核心属性返回带 part 名错误。
  - 在 Ubuntu 24.04 x86_64 VM（rustc 1.98.1）完成 workspace、clippy、fmt、
    oracle 与 15 次 Linux 性能/RSS 基线；LibreOffice 24.2.7.2 代表性 DOCX
    打开并另存 8/8。修复新版 Clippy 报告的 JPEG marker 无需延迟初始化。
  - 在 Windows 11 Pro x64 的 Microsoft Word 16.0.17932.20700 x64 完成 10 个
    代表性 Rust 输出的打开、另存与重开（10/10），人工检查 Word 导出页面
    11/11，并验证重存 DOCX ZIP/XML 结构 10/10。

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
- **P6：Subdoc 子文档合并（ADR-007）**。对齐 docxtpl 0.20.2
  `tpl.new_subdoc(docpath)` + docxcompose 2.2.0 `Composer.attach_parts`；
  新增 5 个 `p6_*` oracle fixture（各带 `*_sub.docx` 子文档），差分
  5 个全部逐字节 MATCH，详见 docs/compatibility.md §4/§7。
  - docxtpl-template：新增 `RenderValue::Subdoc(String)`，经
    `Value::from_safe_string` 注入（对齐 `Subdoc.__html__`，
    autoescape 开/关均原样；片段 jinja 单遍求值、字面标签不二次求值）；
    不提供 From/JSON 入口。
  - docxtpl-xml：新增 `serialize_subtree`（无声明子树序列化）、
    `insert_child_at`（对齐 lxml `element.insert`）、
    `deepcopy_element`（跨文档深拷贝）；主树序列化路径零改动。
  - docxtpl-opc：新增 `ContentTypes::add_override`（Override 保序
    尾插，排序仅在 to_xml）。
  - docxtpl-rs：新增 `crates/docxtpl-rs/src/subdoc.rs`（约 1700 行）
    1:1 复刻 attach_parts 编排——引用部件递归复制（partname/rId
    空洞回填、external rel 迁移）、样式三分支合并（name 映射复用/
    deepcopy append + 编号与 linked 链 fall-through 改写）、编号复制
    （num 尾插前位、anum 首位、映射残留语义）、图片合并（扩展名取
    源 part 后缀、CT 取源包声明、字节 sha1 去重）、页眉页脚引用
    剥离、bookmark/docPr/cNvPr 重编号、分节守卫；树 part dirty 门控
    保留未改字节，图片/主 rels 经 `ImageInjections` 随 finish 落定。
  - 新门面 API `RenderSession::new_subdoc(path) ->
    Result<RenderValue, Error>`（会话内可多次调用，须先于 finish）；
    DEV-0008～DEV-0012：无 docpath 借用模式、custom.xml、编号
    非确定路径（nsid/主缺 numbering/restart 实触发）、
    SmartArt/VML/脚注引用、两侧多节均为不支持并返回带 part 名错误。
  - oracle 侧锁定 docxcompose==2.2.0（tests/oracle/requirements.txt）。
- **P7：媒体/嵌入替换族与模板自省（ADR-008）**。对齐 docxtpl 0.20.2
  `replace_media`/`replace_embedded`/`replace_zipname`/`replace_pic`/
  `reset_replacements` 与 `get_undeclared_template_variables`；新增 7 个
  `p7_*` oracle fixture（含 1 个 `skip_render` 不渲染直存、8 个固定字节
  替换素材），差分 6 个逐字节 MATCH + 1 个错误类别一致
  （ValueError），render fixture 基线达 85（81 MATCH + 4 错误类别），
  golden 85/83，详见 docs/compatibility.md §4/§7。
  - docxtpl-template：新增公开函数
    `find_undeclared_variables(doc_xml, story_xmls) ->
    Result<BTreeSet<String>, _>`（body 与 story 分别 patch 后拼接，
    minijinja 元分析，循环变量自动排除）；新增
    `TemplateErrorKind::InvalidArgument`（oracle 异常 ValueError）。
  - docxtpl-rs：新增 `crates/docxtpl-rs/src/replacements.rs`，
    `Replacements` 四张注册表（media/embedded key=CRC32、zipname
    精确全名、pics 保序 Vec 对齐上游 dict 插入序/命中即 break）；
    pre 路径 `apply_pic_replacements`（主文档 + 主 rels 中
    header/footer 目标出现序不去重；仅 pic:graphicData；结构缺失
    整体跳过；缺失标识 ValueError）与 post 路径
    `apply_byte_replacements`（zipname 精确 > media CRC >
    embeddings CRC，仅换 blob）；新门面 API
    `RenderSession::replace_media/replace_embedded/replace_zipname/
    replace_pic/reset_replacements`（链式）、
    `RenderSession::finish_without_render()`（不渲染直存，不跑
    fix_tables/fix_docpr_ids）、`DocxTemplate::undeclared_variables()`；
    DEV-0013：自省不提供 context 差集/自定义 jinja_env、
    allow_missing_pics 恒为 False。
  - 新依赖 `crc32fast = "1.5"`（与 Python `binascii.crc32` 同
    IEEE 多项式，fixture 差分钉死）。
- **P7b：docxtpl 0.20.2 真实 Word 模板语料字节对齐（ADR-009）**。
  引入 16 个上游仓库真实 Word 模板（LGPL-2.1）作为 `p7b_*` render
  fixture（其中 4 个 python context），关闭 6 个阻断差异族中的 5 个
  （B6 富文本/eastAsia 零改动即支持）；render fixture 基线达 101
  （97 MATCH + 4 错误类别），golden 101/99，85 存量零回归，详见
  docs/compatibility.md §4/§7。
  - docxtpl-template：新增公开 `normalize_part_xml(src, part_name)`
    （strict 解析 + strip_blank_text + 恒单引号 lxml 声明），正文/
    页眉页脚 patch 前输入做 oxml 树往返（实体解码、缩进剥除）；
    脚注路径不做树往返（通用 Part 原字节直穿，保留 Word 双引号
    声明）；渲染字符串入口统一 CRLF/CR→LF（对齐 Jinja2 lexer
    tnewline）；修复 `{_% %_}` 字面转义还原错字（`{_%`→`{%`）。
  - docxtpl-opc：新增 `ContentTypes::rebuild_from_parts`（移植
    python-docx spec.py 默认内容类型表，rels/xml Default 恒在、
    rels Override 消失）与 `Package::rebuild_content_types`、
    `Package::normalize_relationships`（根/挂接 rels 全部重写为
    规范字节，字节未变不写回）。
  - docxtpl-rs：保存前归一升级为三步——styles/settings/numbering
    已知 XmlPart 树往返（通用 Part blob 透传）→ rels 归一 → CT
    from_parts 重建；render() 与 RenderSession::finish 共用。
  - docxtpl-xml：正文序列化路径模拟 lxml 跨树换挂的命名空间归并
    （元素局部 xmlns 与祖先同 URI 时丢弃声明，元素名/属性名/后代
    前缀重绑到祖先前缀；页眉页脚整树 parse/tostring 路径保持
    词法，ADR-006）。
  - tests：`generate.py` 新增 `source`/`source_upstream` 机制
    （从 `tests/fixtures/sources/` 复制真实模板不程序化 build，
    manifest 记录上游出处）；oracle_diff 新增 4 个 P7b Rust
    python-context arms；逐 part 字节/c14n 差分为主，P7d 后续补齐
    Microsoft Word 与 LibreOffice 代表性实机抽查。

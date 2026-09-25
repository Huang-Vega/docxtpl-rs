//! docxtpl-rs：将 docx 文件当作 Jinja2 模板的 Rust 实现。
//!
//! 公开门面：[`DocxTemplate::open`] → [`DocxTemplate::render`] /
//! [`DocxTemplate::render_ctx`] → [`RenderedDocument::save`]。语义对齐固定
//! 基线 Python docxtpl 0.20.2（见 docs/compatibility.md 与 ADR-001/005）。
//!
//! 纯 JSON 上下文：
//!
//! ```no_run
//! use docxtpl_rs::{DocxTemplate, RenderOptions};
//! use serde_json::json;
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let tpl = DocxTemplate::open("template.docx")?;
//! let doc = tpl.render(
//!     &json!({"name": "Vega", "items": [{"name": "Apple"}]}),
//!     &RenderOptions::compat(),
//! )?;
//! doc.save("output.docx")?;
//! # Ok(()) }
//! ```
//!
//! 富内容（RichText/Listing/InlineImage）上下文：
//!
//! ```no_run
//! use docxtpl_rs::{DocxTemplate, InlineImage, RenderContext, RenderOptions};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let tpl = DocxTemplate::open("template.docx")?;
//! let mut ctx = RenderContext::new();
//! ctx.insert("img", InlineImage::from_path("photo.png", None, None, None)?);
//! let doc = tpl.render_ctx(&ctx, &RenderOptions::compat())?;
//! doc.save("output.docx")?;
//! # Ok(()) }
//! ```
//!
//! 渲染前需要外部超链接 rId（上游 `tpl.build_url_id(url)`）时，使用
//! [`DocxTemplate::render_session`] 开启一次性会话：
//!
//! ```no_run
//! use docxtpl_rs::{DocxTemplate, RenderContext, RenderOptions, RichText, RichTextProps};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let tpl = DocxTemplate::open("template.docx")?;
//! let mut session = tpl.render_session(&RenderOptions::compat())?;
//! let url_id = session.build_url_id("https://example.com/");
//! let mut props = RichTextProps::new();
//! props.url_id = Some(url_id);
//! let mut ctx = RenderContext::new();
//! ctx.insert("rt", RichText::text_with("链接", &props));
//! let doc = session.finish(&ctx)?;
//! doc.save("output.docx")?;
//! # Ok(()) }
//! ```
//!
//! [`DocxTemplate`] 只读且可复用：每次渲染都从原始模板字节重新开包，
//! 渲染临时状态不会跨次污染。

use std::collections::BTreeSet;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use docxtpl_opc::{resolve_part_target, OpcError, Package, PackageLimits, PartUri, TargetMode};
use docxtpl_template::{
    find_undeclared_variables, normalize_part_xml, render_core_properties_ctx,
    render_document_xml_ctx, render_footnotes_xml_ctx, render_story_xml_ctx, RenderError,
};
// 同时作为内部类型与对外重导出（底部 pub use 列表不再重复）。
pub use docxtpl_template::RenderContext;

mod images;
mod replacements;
mod subdoc;

use images::ImageInjections;
use replacements::Replacements;

const MAX_INPUT_DOCX_BYTES: u64 = 128 * 1024 * 1024;

/// footnotes part 的 content type（上游 render_footnotes 按此过滤
/// package.parts；该 CT 未在 python-docx PartFactory 注册，故按通用
/// 二进制 part 处理，渲染结果原样写回不重序列化，ADR-006）。
const CT_FOOTNOTES: &str =
    "application/vnd.openxmlformats-officedocument.wordprocessingml.footnotes+xml";

/// python-docx PartFactory 注册为已知 XmlPart 子类、保存时**恒由 lxml 树
/// 重序列化**的 part content type（P7b B1）：document/header/footer/core
/// 已在渲染管线中重写；styles/settings/numbering 即使渲染未触碰也须做
/// 树形态归一（真实 Word 模板里它们是双引号声明 + CRLF 的 Word 形态）。
/// fontTable/webSettings/theme/footnotes/endnotes/comments/customXml 等
/// 未注册的通用 Part 继续 blob 透传（DEV 分类见 compatibility.md）。
const ALWAYS_REWRITTEN_XML_CTS: &[&str] = &[
    "application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.settings+xml",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml",
];

/// 一个可复用的 docx 模板（只读持有模板字节）。
pub struct DocxTemplate {
    data: Vec<u8>,
}

impl DocxTemplate {
    /// 从文件打开模板（读取全部字节并做基础可读校验，不渲染）。
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        Self::from_reader(std::fs::File::open(path)?)
    }

    /// 从任意读取器读入模板。
    pub fn from_reader(reader: impl Read) -> Result<Self, Error> {
        let mut data = Vec::new();
        reader
            .take(MAX_INPUT_DOCX_BYTES + 1)
            .read_to_end(&mut data)?;
        Self::from_bytes(data)
    }

    /// 从已有字节构造模板；会立即按 OPC 限额做一次完整解析与校验，
    /// 确保后续 render 不会因包结构问题失败。
    pub fn from_bytes(data: Vec<u8>) -> Result<Self, Error> {
        if data.len() as u64 > MAX_INPUT_DOCX_BYTES {
            return Err(Error::InputTooLarge {
                max: MAX_INPUT_DOCX_BYTES,
            });
        }
        let pkg = Package::from_reader(Cursor::new(&data), &PackageLimits::default())?;
        pkg.validate()?;
        Ok(Self { data })
    }

    /// 用纯 JSON 上下文渲染，返回独立的 [`RenderedDocument`]。
    ///
    /// 按上游固定顺序渲染全部 part（P5，ADR-006）：正文 → 页眉 → 页脚 →
    /// 核心属性 → 脚注；未修改 part 原样保留。失败返回带 part 与行号
    /// 上下文的 [`RenderError`]。
    pub fn render(
        &self,
        context: &serde_json::Value,
        options: &RenderOptions,
    ) -> Result<RenderedDocument, Error> {
        // 每次渲染独立开包：DocxTemplate 可复用，状态不跨次（规范 §2.2）。
        let mut pkg = self.open_package()?;
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let context = RenderContext::from_json(context);
        let mut injections = ImageInjections::new(&pkg, &main_name)?;

        render_all_parts(&mut pkg, &context, options, &mut injections)?;
        // JSON 上下文不含图片/外链，apply 实际为 no-op（无脏作用域）。
        injections.apply(&mut pkg)?;
        canonicalize_content_types(&mut pkg)?;

        // 写出前再过一次包校验：不得产生悬空关系/缺 part。
        pkg.validate()?;

        Ok(RenderedDocument { pkg })
    }

    /// 用富内容上下文渲染（P4，ADR-005）：上下文可含 RichText /
    /// RichTextParagraph / Listing / InlineImage。
    ///
    /// 图片的 media part、document rels 与 [Content_Types].xml 变更在渲染
    /// 结束后一次性落定；不涉及图片/外链时这些 part 保持原字节。
    /// 需要渲染前预登记外部超链接（上游 `tpl.build_url_id`）时改用
    /// [`DocxTemplate::render_session`]。
    pub fn render_ctx(
        &self,
        context: &RenderContext,
        options: &RenderOptions,
    ) -> Result<RenderedDocument, Error> {
        self.render_session(options)?.finish(context)
    }

    /// 开启一次性富内容渲染会话：会话内可先 [`RenderSession::build_url_id`]
    /// 预登记外部超链接关系（对齐上游在渲染前调用 `tpl.build_url_id`），
    /// 再 [`RenderSession::finish`] 完成渲染。
    pub fn render_session(&self, options: &RenderOptions) -> Result<RenderSession, Error> {
        let pkg = self.open_package()?;
        RenderSession::new(pkg, options)
    }

    /// 上游 `DocxTemplate.get_undeclared_template_variables()`（P7）：
    /// 对模板正文 body 与主 rels 中全部非空页眉/页脚做 patch_xml 后做
    /// jinja 元分析，返回模板引用但未被 `{% for %}`/宏参数等声明的
    /// 变量名集合（排序返回以便稳定展示）。
    ///
    /// 纯静态自省，不修改模板、不产生输出文档；上游可选的 `context`
    /// 差集参数未提供，调用方可对返回集合自行做差集。
    pub fn undeclared_variables(&self) -> Result<BTreeSet<String>, Error> {
        let pkg = self.open_package()?;
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let doc_xml = read_part_utf8(&pkg, &main_name)?;
        // 复用渲染的 story 枚举：先 header 后 footer、内部目标、blob 非空。
        // 上游 rels 遍历不去重，但求并集时去重不影响结果。
        let story_xmls = story_parts(&pkg, &main_name)?
            .iter()
            .map(|name| read_part_utf8(&pkg, name))
            .collect::<Result<Vec<_>, _>>()?;
        find_undeclared_variables(&doc_xml, &story_xmls).map_err(Error::Render)
    }

    /// 每次渲染独立开包并校验。
    fn open_package(&self) -> Result<Package, Error> {
        let pkg = Package::from_reader(Cursor::new(&self.data), &PackageLimits::default())?;
        pkg.validate()?;
        Ok(pkg)
    }
}

/// 一次富内容渲染会话（ADR-005）：持有打开的包与图片/关系注入状态。
///
/// 对齐上游 `DocxTemplate` 实例在单次 render 内的可变性：
/// `build_url_id` 预登记的外链条目与渲染期解析的图片共用同一份 document
/// rels，rId 按调用顺序回填/尾插。会话一次性消费（[`RenderSession::finish`]
/// 取走所有权），不跨渲染复用。
pub struct RenderSession {
    pkg: Package,
    options: RenderOptions,
    injections: ImageInjections,
    /// P7 媒体/嵌入替换注册表（replace_* 系列，finish 时落定）。
    replacements: Replacements,
}

impl RenderSession {
    /// 定位主文档并构造图片注入注册表（主文档 rels 缺失时报 OPC 错误）。
    fn new(pkg: Package, options: &RenderOptions) -> Result<Self, Error> {
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let injections = ImageInjections::new(&pkg, &main_name)?;
        Ok(Self {
            pkg,
            options: *options,
            injections,
            replacements: Replacements::new(),
        })
    }

    /// 上游 `DocxTemplate.build_url_id`：登记（或复用）外部超链接关系，
    /// 返回其 rId。同 URL（reltype+External）幂等复用既有关系。
    ///
    /// 必须在 [`RenderSession::finish`] 之前、构造引用该 rId 的
    /// [`RichText`] 之前调用。
    #[must_use]
    pub fn build_url_id(&mut self, url: &str) -> String {
        self.injections.build_url_id(url)
    }

    /// 上游 `DocxTemplate.new_subdoc(docpath)`（P6，ADR-007）：打开外部
    /// docx 并把其部件合并进当前主包，返回可直接插入上下文的 Subdoc 值
    /// （模板位写法 `{{p sd }}`）。
    ///
    /// 合并内容：引用部件复制（rels/Content Types）、样式三分支合并、
    /// 编号复制、图片 media 去重并入、主文档 bookmark/docPr/cNvPr 重编号。
    /// 树部件变更即时落包（未改 part 保留原字节）；图片与主 rels 变更在
    /// [`RenderSession::finish`] 时与渲染结果一起落定。
    ///
    /// 必须在 [`RenderSession::finish`] 之前调用，且每次渲染会话内可调用
    /// 多次（对齐上游构造期合并的幂等性）。
    pub fn new_subdoc(
        &mut self,
        docpath: impl AsRef<std::path::Path>,
    ) -> Result<RenderValue, Error> {
        subdoc::new_subdoc(&mut self.pkg, &mut self.injections, docpath.as_ref())
    }

    /// 上游 `DocxTemplate.replace_media`（P7）：注册按源字节 CRC32
    /// 命中的 media 替换（`word/media/` 条目，含正文/页眉/页脚图片）。
    ///
    /// 仅换最终 zip 条目字节：part 名、[Content_Types].xml、rels 与
    /// wp:extent 版式均保持模板原样。源/目标传已读入的字节（上游的
    /// 文件路径/file-like 两种形态在此统一为字节）。
    pub fn replace_media(&mut self, src: impl AsRef<[u8]>, dst: impl AsRef<[u8]>) -> &mut Self {
        self.replacements.replace_media(src.as_ref(), dst.as_ref());
        self
    }

    /// 上游 `DocxTemplate.replace_embedded`（P7）：注册按源字节 CRC32
    /// 命中的嵌入对象替换（`word/embeddings/` 条目）。
    pub fn replace_embedded(&mut self, src: impl AsRef<[u8]>, dst: impl AsRef<[u8]>) -> &mut Self {
        self.replacements
            .replace_embedded(src.as_ref(), dst.as_ref());
        self
    }

    /// 上游 `DocxTemplate.replace_zipname`（P7）：按 zip 条目全名
    /// （不带前导 `/`，如 `word/embeddings/x.xlsx`）精确替换。
    pub fn replace_zipname(&mut self, zipname: &str, dst: impl AsRef<[u8]>) -> &mut Self {
        self.replacements.replace_zipname(zipname, dst.as_ref());
        self
    }

    /// 上游 `DocxTemplate.replace_pic`（P7）：按图片 cNvPr 的
    /// name/title/descr 标识注册替换，作用于渲染后正文与页眉/页脚
    /// pic 图形引用的 media part blob。
    ///
    /// 标识在所有扫描 part 中均未命中时，[`RenderSession::finish`] /
    /// [`RenderSession::finish_without_render`] 返回
    /// `ValueError` 类错误（[`docxtpl_template::TemplateErrorKind::InvalidArgument`]）。
    pub fn replace_pic(&mut self, pic_id: &str, dst: impl AsRef<[u8]>) -> &mut Self {
        self.replacements.replace_pic(pic_id, dst.as_ref());
        self
    }

    /// 上游 `DocxTemplate.reset_replacements`（P7）：清空本会话全部
    /// media/embedded/zipname/pic 替换注册。
    pub fn reset_replacements(&mut self) -> &mut Self {
        self.replacements.reset();
        self
    }

    /// 渲染全部 part（正文 → 页眉 → 页脚 → 核心属性 → 脚注，P5），
    /// 落定 media part / 各作用域 rels / Content Types 变更，并做最终包校验。
    pub fn finish(mut self, context: &RenderContext) -> Result<RenderedDocument, Error> {
        render_all_parts(&mut self.pkg, context, &self.options, &mut self.injections)?;

        // 先写 media part，再回写各 owner rels/CT，保证最终校验无悬空关系/类型。
        self.injections.apply(&mut self.pkg)?;
        self.finish_replacements()
    }

    /// 上游「不 render 直接 save」路径（P7，save() L887–889）：不跑
    /// 模板渲染管线（不 patch/不渲染、不 fix_tables/fix_docpr_ids，
    /// docPr id 等保持模板原字节），仅执行 pre/post 替换后落盘。
    ///
    /// 对齐上游 `is_rendered=False` 时 save 重新打开模板的语义：本
    /// 出口不应与 [`RenderSession::build_url_id`] /
    /// [`RenderSession::new_subdoc`] 混用（其暂存/合并内容不应用）。
    pub fn finish_without_render(self) -> Result<RenderedDocument, Error> {
        self.finish_replacements()
    }

    /// pre_processing（replace_pic）→ CT 归一（python-docx 每次保存都
    /// 重建 CT）→ post_processing（CRC/zipname 字节替换）→ 最终校验。
    fn finish_replacements(mut self) -> Result<RenderedDocument, Error> {
        let main_name = self.pkg.main_document_uri()?.as_str().to_string();
        // pre_processing：在最终 XML 上换图片 part blob（docx.save 之前）。
        self.replacements
            .apply_pic_replacements(&mut self.pkg, &main_name)?;
        canonicalize_content_types(&mut self.pkg)?;
        // post_processing：最终 part 集合上的 CRC/zipname 字节替换。
        self.replacements.apply_byte_replacements(&mut self.pkg)?;
        self.pkg.validate()?;

        Ok(RenderedDocument { pkg: self.pkg })
    }
}

/// 上游 `DocxTemplate.render` 的 part 编排（顺序固定，ADR-006）：
/// 正文 → 页眉（主文档 rels 序）→ 页脚（主文档 rels 序）→
/// 核心属性 → footnotes（按包 part 枚举的 CT 过滤）。
///
/// 页眉/页脚的图片关系分配在各自 part 的作用域
/// （[`ImageInjections::begin_owner`]）；脚注不允许图片且输出不重序列化。
/// 变更只写回被渲染过的 part；rels/CT 的落定由调用方在渲染后执行
/// [`ImageInjections::apply`] 完成。
fn render_all_parts(
    pkg: &mut Package,
    context: &RenderContext,
    options: &RenderOptions,
    injections: &mut ImageInjections,
) -> Result<(), Error> {
    let main_name = pkg.main_document_uri()?.as_str().to_string();

    // 1. 正文：fix_tables + fix_docpr_ids，图片关系归属主文档。
    injections.begin_owner(pkg, &main_name)?;
    let body_src = read_part_utf8(pkg, &main_name)?;
    let body = render_document_xml_ctx(&body_src, context, options, injections)?;
    pkg.set_part_bytes(&main_name, body.xml.into_bytes())?;

    // 2/3. 页眉、页脚（两遍 rels 枚举，顺序对齐
    // build_headers_footers_xml(HEADER_URI) 再 (FOOTER_URI)）：lxml 往返、
    // resolve_listing 照跑，但不做 fix_tables / fix_docpr_ids。
    let stories = story_parts(pkg, &main_name)?;
    for name in stories {
        injections.begin_owner(pkg, &name)?;
        let src = read_part_utf8(pkg, &name)?;
        let outcome = render_story_xml_ctx(&src, context, options, injections, &name)?;
        pkg.set_part_bytes(&name, outcome.xml.into_bytes())?;
    }

    // 4. 核心属性：上游 render() 无条件执行 render_properties。目标经根
    // rels 的 core-properties 关系解析（标准 docx 即 docProps/core.xml）。
    if let Some(core_name) = core_properties_part(pkg) {
        let core_src = read_part_utf8(pkg, &core_name)?;
        let rendered =
            render_core_properties_ctx(&core_src, context, options.autoescape(), injections)?;
        pkg.set_part_bytes(&core_name, rendered.into_bytes())?;
    }

    // 5. 脚注：通用二进制 part，渲染字符串原样写回（保留 XML 声明）。
    for name in footnotes_parts(pkg) {
        let src = read_part_utf8(pkg, &name)?;
        let rendered = render_footnotes_xml_ctx(&src, context, options, &name)?;
        pkg.set_part_bytes(&name, rendered.into_bytes())?;
    }

    Ok(())
}

/// 保存前包级形态归一（P7b B1，对齐 python-docx `PackageWriter.write`）：
///
/// 1. 已知 XmlPart（styles/settings/numbering）恒由 lxml 树重序列化
///    （[`ALWAYS_REWRITTEN_XML_CTS`]）；通用 Part blob 透传；
/// 2. 全部 `.rels`（含根 rels 与 customXml 子 rels）恒重写为模型规范
///    XML（单引号声明）；
/// 3. `[Content_Types].xml` 按 `_ContentTypesItem.from_parts` 重建：
///    rels Override 消失、rels/xml Default 恒在，命中扩展名默认表的
///    part 落 Default、其余落 Override，排序后写回。
///
/// 三类归一均为字节未变化时不写回。
fn canonicalize_content_types(pkg: &mut Package) -> Result<(), Error> {
    normalize_known_xml_parts(pkg)?;
    pkg.normalize_relationships()?;

    pkg.rebuild_content_types();
    let canonical = pkg.content_types().to_xml().into_bytes();
    let unchanged = pkg
        .part("[Content_Types].xml")
        .is_some_and(|part| part.bytes() == canonical.as_slice());
    if !unchanged {
        pkg.set_part_bytes("[Content_Types].xml", canonical)?;
    }
    Ok(())
}

/// 对 styles/settings/numbering 等已知 XmlPart 做无条件树形态归一
/// （python-docx 保存时这些 part 恒重序列化，即使渲染未触碰）。
fn normalize_known_xml_parts(pkg: &mut Package) -> Result<(), Error> {
    // 先定位目标 part 名，再读+归一，避免遍历借用与 set_part_bytes 冲突。
    let names: Vec<String> = pkg
        .parts()
        .filter(|part| {
            !part.is_dir()
                && pkg
                    .content_types()
                    .content_type_of(part.uri())
                    .is_some_and(|ct| ALWAYS_REWRITTEN_XML_CTS.contains(&ct))
        })
        .map(|part| part.name().to_string())
        .collect();
    let mut pending: Vec<(String, Vec<u8>)> = Vec::with_capacity(names.len());
    for name in &names {
        let src = read_part_utf8(pkg, name)?;
        let normalized = normalize_part_xml(&src, name).map_err(Error::Render)?;
        pending.push((name.clone(), normalized.into_bytes()));
    }
    for (name, bytes) in pending {
        let unchanged = pkg
            .part(&name)
            .is_some_and(|part| part.bytes() == bytes.as_slice());
        if !unchanged {
            pkg.set_part_bytes(&name, bytes)?;
        }
    }
    Ok(())
}

/// 枚举主文档 rels 中的内部 header/footer 目标（P5）。
///
/// 对齐上游 `get_headers_footers` + 两遍 build：顺序为主文档 rels 解析序，
/// 先全部 header 再全部 footer；只收 internal 关系、目标 part 存在且
/// blob 非空；同一目标 part 去重（病态双 rel 场景见 DEV-0007）。
fn story_parts(pkg: &Package, main_name: &str) -> Result<Vec<String>, Error> {
    let main_uri = PartUri::new(main_name)?;
    // 相对 Target 相对的是主文档 part 所在目录（word/），不是 part 路径本身。
    let base_dir = main_uri.parent();
    let Some(rels) = pkg.relationships_of(main_name) else {
        return Ok(Vec::new());
    };
    let mut parts = Vec::new();
    for rel_type_suffix in ["/header", "/footer"] {
        for rel in rels.iter() {
            if rel.target_mode != TargetMode::Internal || !rel.rel_type.ends_with(rel_type_suffix) {
                continue;
            }
            let Some(target) = resolve_part_target(base_dir.as_ref(), &rel.target) else {
                continue;
            };
            let name = target.as_str();
            let Some(part) = pkg.part(name) else {
                continue;
            };
            // 上游 `if val.target_part.blob`：空 blob 跳过。
            if part.bytes().is_empty() || parts.iter().any(|existing| existing == name) {
                continue;
            }
            parts.push(name.to_string());
        }
    }
    Ok(parts)
}

/// 枚举包内 footnotes part（P5）：遍历包 part，content type 为
/// wordprocessingml.footnotes+xml（endnotes 不在范围，DEV-0007）。
fn footnotes_parts(pkg: &Package) -> Vec<String> {
    pkg.parts()
        .map(|part| part.uri())
        .filter(|uri| pkg.content_types().content_type_of(uri) == Some(CT_FOOTNOTES))
        .map(|uri| uri.as_str().to_string())
        .collect()
}

/// 读取 part 字节并按 UTF-8 解码（模板 XML 必须为 UTF-8 文本）。
fn read_part_utf8(pkg: &Package, name: &str) -> Result<String, Error> {
    let bytes = pkg
        .part(name)
        .ok_or_else(|| OpcError::MissingPart {
            uri: name.to_string(),
        })?
        .bytes()
        .to_vec();
    std::str::from_utf8(&bytes)
        .map(str::to_owned)
        .map_err(|source| Error::NotUtf8 {
            part: name.to_string(),
            source,
        })
}

/// 经根 rels 的 core-properties 关系定位核心属性 part（仅内部目标）。
fn core_properties_part(pkg: &Package) -> Option<String> {
    pkg.root_relationships()
        .iter()
        .find(|r| r.rel_type.ends_with("/metadata/core-properties"))
        .filter(|r| r.target_mode == TargetMode::Internal)
        .map(|r| r.target.clone())
        .filter(|name| pkg.contains(name))
}

/// 一次渲染的结果文档。
pub struct RenderedDocument {
    pkg: Package,
}

impl RenderedDocument {
    /// 保存到文件（未修改 part 保持原始字节，见 ADR-002 DEV-0004）。
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        self.pkg.save(path)?;
        Ok(())
    }

    /// 写入任意可定位写入器。
    pub fn write_to(&self, writer: impl Write + std::io::Seek) -> Result<(), Error> {
        self.pkg.write_to(writer)?;
        Ok(())
    }

    /// 序列化为内存中的 docx 字节。
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let mut buf = Cursor::new(Vec::new());
        self.pkg.write_to(&mut buf)?;
        Ok(buf.into_inner())
    }
}

impl Error {
    /// 模板错误的稳定类别（OPC/IO 等错误为 None）。
    #[must_use]
    pub fn kind(&self) -> Option<TemplateErrorKind> {
        match self {
            Error::Render(e) => e.kind(),
            _ => None,
        }
    }
}

/// 门面层错误：OPC、模板渲染或编码问题。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 压缩后的输入文件超过默认读取限额。
    #[error("输入 DOCX 超过 {max} 字节限额")]
    InputTooLarge { max: u64 },
    /// ZIP/OPC 包错误（限额、URI、关系等）。
    #[error(transparent)]
    Opc(#[from] OpcError),

    /// 模板渲染错误（语法/上下文/XML 愈合/后处理）。
    #[error(transparent)]
    Render(#[from] RenderError),

    /// part 不是合法 UTF-8（模板 XML 必须为 UTF-8 文本）。
    #[error("part {part} 不是合法 UTF-8: {source}")]
    NotUtf8 {
        /// 出错的 part 名。
        part: String,
        /// 底层编码错误。
        #[source]
        source: std::str::Utf8Error,
    },

    /// 文件 IO 错误。
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub use docxtpl_rich::{InlineImage, Listing, RichText, RichTextParagraph, RichTextProps};
pub use docxtpl_template::{RenderOptions, RenderValue, TemplateErrorKind};

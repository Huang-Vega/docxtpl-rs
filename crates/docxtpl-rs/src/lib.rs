//! docxtpl-rs：将 docx 文件当作 Jinja2 模板的 Rust 实现。
//!
//! 公开门面：[`DocxTemplate::open`] → [`DocxTemplate::render`] →
//! [`RenderedDocument::save`]。语义对齐固定基线 Python docxtpl 0.20.2
//! （见 docs/compatibility.md 与 ADR-001）。
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
//! [`DocxTemplate`] 只读且可复用：每次 [`DocxTemplate::render`] 都从原始
//! 模板字节重新开包，渲染临时状态不会跨次污染。

use std::io::{Cursor, Read, Write};
use std::path::Path;

use docxtpl_opc::{OpcError, Package, PackageLimits, TargetMode};
use docxtpl_template::RenderError;

const MAX_INPUT_DOCX_BYTES: u64 = 128 * 1024 * 1024;

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

    /// 用给定上下文渲染，返回独立的 [`RenderedDocument`]。
    ///
    /// 当前阶段（P2/P3）只渲染主文档 part（word/document.xml）；
    /// 未修改 part 原样保留。失败返回带 part 与行号上下文的 [`RenderError`]。
    pub fn render(
        &self,
        context: &serde_json::Value,
        options: &RenderOptions,
    ) -> Result<RenderedDocument, Error> {
        // 每次渲染独立开包：DocxTemplate 可复用，状态不跨次（规范 §2.2）。
        let mut pkg = Package::from_reader(Cursor::new(&self.data), &PackageLimits::default())?;
        pkg.validate()?;

        let main_uri = pkg.main_document_uri()?;
        let main_name = main_uri.as_str().to_string();
        let original = pkg
            .part(&main_name)
            .ok_or_else(|| OpcError::MissingPart {
                uri: main_name.clone(),
            })?
            .bytes()
            .to_vec();
        let src_xml = std::str::from_utf8(&original).map_err(|e| Error::NotUtf8 {
            part: main_name.clone(),
            source: e,
        })?;

        let outcome = docxtpl_template::render_document_xml(src_xml, context, options)?;
        pkg.set_part_bytes(&main_name, outcome.xml.into_bytes())?;

        // 核心属性：上游 render() 无条件执行 render_properties（会补齐
        // dc:identifier/dc:language 等元素）。目标经根 rels 的
        // core-properties 关系解析（标准 docx 即 docProps/core.xml）。
        let core_target: Option<(String, TargetMode)> = pkg
            .root_relationships()
            .iter()
            .find(|r| r.rel_type.ends_with("/metadata/core-properties"))
            .map(|r| (r.target.clone(), r.target_mode));
        if let Some((core_name, mode)) = core_target {
            if matches!(mode, TargetMode::Internal) && pkg.contains(&core_name) {
                let core_bytes = pkg
                    .part(&core_name)
                    .ok_or_else(|| OpcError::MissingPart {
                        uri: core_name.clone(),
                    })?
                    .bytes()
                    .to_vec();
                let core_src = std::str::from_utf8(&core_bytes).map_err(|e| Error::NotUtf8 {
                    part: core_name.clone(),
                    source: e,
                })?;
                let rendered_core = docxtpl_template::render_core_properties(
                    core_src,
                    context,
                    options.autoescape(),
                )?;
                pkg.set_part_bytes(&core_name, rendered_core.into_bytes())?;
            }
        }

        // 写出前再过一次包校验：不得产生悬空关系/缺 part。
        pkg.validate()?;

        Ok(RenderedDocument { pkg })
    }
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

pub use docxtpl_template::{RenderOptions, TemplateErrorKind};

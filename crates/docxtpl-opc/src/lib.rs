//! docxtpl-opc：OOXML/OPC 包读写层。
//!
//! 负责 ZIP 读写、part URI、[Content_Types].xml 与 relationship 索引、
//! 输入限额与包完整性校验。本 crate 不理解 Jinja 标签，也不做模板语义。
//!
//! 典型流程：[`Package::open`]（在 [`PackageLimits`] 约束下安全读入）→
//! 读取/校验 → [`Package::set_part_bytes`] 修改 → [`Package::save`] /
//! [`Package::write_to`] 写回。
//!
//! # 示例
//!
//! 构造一个最小 docx 并完整走一遍“打开 → 校验 → 修改 → 写回 → 重开”：
//!
//! ```
//! use std::io::{Cursor, Write};
//!
//! use docxtpl_opc::{Package, PackageLimits};
//! use zip::write::{SimpleFileOptions, ZipWriter};
//!
//! // 1. 构造最小合法 OPC 包
//! let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
//! writer.start_file("[Content_Types].xml", SimpleFileOptions::default())?;
//! writer.write_all(br#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
//!   <Default Extension="xml" ContentType="application/xml"/>
//!   <Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
//! </Types>"#)?;
//! writer.start_file("_rels/.rels", SimpleFileOptions::default())?;
//! writer.write_all(br#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
//!   <Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
//! </Relationships>"#)?;
//! writer.start_file("word/document.xml", SimpleFileOptions::default())?;
//! writer.write_all(b"<w:document><w:body/></w:document>")?;
//! let docx = writer.finish()?.into_inner();
//!
//! // 2. 打开（限额先于读字节检查，拦截 zip 炸弹）
//! let mut pkg = Package::from_reader(Cursor::new(docx), &PackageLimits::default())?;
//! assert_eq!(pkg.part_count(), 3);
//! assert_eq!(pkg.main_document_uri()?.as_str(), "word/document.xml");
//! pkg.validate()?;
//!
//! // 3. 修改主文档并写回
//! pkg.set_part_bytes("word/document.xml", b"<w:document/>".to_vec())?;
//! let ct_before = pkg.part("[Content_Types].xml").unwrap().bytes().to_vec();
//! let mut out = Vec::new();
//! pkg.write_to(Cursor::new(&mut out))?;
//!
//! // 4. 重开：修改生效，未修改的 part 字节保持不变
//! let reopened = Package::from_reader(Cursor::new(&out), &PackageLimits::default())?;
//! assert_eq!(reopened.part("word/document.xml").unwrap().bytes(), b"<w:document/>");
//! assert_eq!(reopened.part("[Content_Types].xml").unwrap().bytes(), ct_before);
//! # Ok::<(), Box<dyn std::error::Error>>(())
//! ```

mod content_types;
mod error;
mod limits;
mod package;
mod part;
mod rels;
mod uri;
mod xmlutil;

pub use crate::content_types::ContentTypes;
pub use crate::error::OpcError;
pub use crate::limits::PackageLimits;
pub use crate::package::Package;
pub use crate::part::Part;
pub use crate::rels::{Relationship, Relationships, TargetMode};
pub use crate::uri::PartUri;

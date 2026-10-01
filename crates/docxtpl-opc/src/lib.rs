//! docxtpl-opc: OOXML/OPC package reader/writer layer.
//!
//! Handles ZIP reading/writing, part URIs, `[Content_Types].xml` and
//! relationship indexes, input limits, and package integrity validation.
//! This crate does not understand Jinja tags or implement template semantics.
//!
//! Typical flow: [`Package::open`] (read safely under [`PackageLimits`]) →
//! read/validate → [`Package::set_part_bytes`] to modify → [`Package::save`] /
//! [`Package::write_to`] to write back.
//!
//! # Examples
//!
//! Build a minimal docx and walk through the full
//! "open → validate → modify → write back → reopen" cycle:
//!
//! ```
//! use std::io::{Cursor, Write};
//!
//! use docxtpl_opc::{Package, PackageLimits};
//! use zip::write::{SimpleFileOptions, ZipWriter};
//!
//! // 1. Build a minimal valid OPC package
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
//! // 2. Open (limits are checked before reading bytes, blocking zip bombs)
//! let mut pkg = Package::from_reader(Cursor::new(docx), &PackageLimits::default())?;
//! assert_eq!(pkg.part_count(), 3);
//! assert_eq!(pkg.main_document_uri()?.as_str(), "word/document.xml");
//! pkg.validate()?;
//!
//! // 3. Modify the main document and write back
//! pkg.set_part_bytes("word/document.xml", b"<w:document/>".to_vec())?;
//! let ct_before = pkg.part("[Content_Types].xml").unwrap().bytes()?.to_vec();
//! let mut out = Vec::new();
//! pkg.write_to(Cursor::new(&mut out))?;
//!
//! // 4. Reopen: the change takes effect, while unmodified part bytes stay unchanged
//! let reopened = Package::from_reader(Cursor::new(&out), &PackageLimits::default())?;
//! assert_eq!(reopened.part("word/document.xml").unwrap().bytes()?, b"<w:document/>");
//! assert_eq!(reopened.part("[Content_Types].xml").unwrap().bytes()?, ct_before);
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
pub use crate::error::{InterruptibleWriteError, OpcError};
pub use crate::limits::PackageLimits;
pub use crate::package::{
    MediaCompression, Package, PackageEvictionReport, PackageResidency, PackageTransaction,
    PackageWriteReport, WriteOptions,
};
pub use crate::part::{FilePartSource, Part};
pub use crate::rels::{Relationship, Relationships, TargetMode};
pub use crate::uri::{relationships_path_of, resolve_part_target, PartUri};

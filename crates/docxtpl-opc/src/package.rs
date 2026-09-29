//! OPC packages: opening (with limits), relationships/Content Types indexes,
//! validation, and writing back.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use zip::read::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

use crate::content_types::ContentTypes;
use crate::error::OpcError;
use crate::limits::PackageLimits;
use crate::part::{LazyArchive, Part};
use crate::rels::{Relationships, TargetMode};
use crate::uri::{
    collapse_segments, is_rels_path, owner_of_rels_path, percent_decode, rels_path_for,
    resolve_relative_to, PartUri,
};

/// officeDocument relationship type (the main document).
const OFFICE_DOCUMENT_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";

/// Compression policy for media entries that must be written from bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MediaCompression {
    /// Preserve the current behavior: Stored entries stay Stored and all other
    /// rewritten media entries use Deflate at the ZIP crate's default level.
    #[default]
    Compatible,
    /// Use Deflate level 1 for rewritten media entries.
    FastDeflate,
    /// Store rewritten media entries without compression.
    Stored,
    /// Store formats that are already compressed and use fast Deflate for
    /// media formats that can still benefit from ZIP compression.
    Auto,
}

/// Options controlling OPC ZIP serialization.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WriteOptions {
    media_compression: MediaCompression,
}

impl WriteOptions {
    /// Options matching the historical writer behavior.
    #[must_use]
    pub const fn compatible() -> Self {
        Self {
            media_compression: MediaCompression::Compatible,
        }
    }

    /// Selects the compression policy for rewritten media entries.
    #[must_use]
    pub const fn with_media_compression(mut self, compression: MediaCompression) -> Self {
        self.media_compression = compression;
        self
    }

    /// Returns the selected media compression policy.
    #[must_use]
    pub const fn media_compression(&self) -> MediaCompression {
        self.media_compression
    }
}

/// An OPC package (the OOXML ZIP container + Content Types + relationships).
///
/// See the crate-level documentation for the typical flow.
pub struct Package {
    limits: PackageLimits,
    source: Option<Arc<LazyArchive>>,
    source_path: Option<PathBuf>,
    parts: Vec<Part>,
    /// Original entry name → index into parts.
    index: HashMap<String, usize>,
    content_types: ContentTypes,
    root_rels: Relationships,
}

impl Package {
    /// Open an OPC package from a file.
    ///
    /// # Failure cases
    ///
    /// [`OpcError::Io`] is returned when the file cannot be opened; otherwise
    /// the same failures as [`Package::from_reader`] apply.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use docxtpl_opc::{Package, PackageLimits};
    ///
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert!(pkg.contains("[Content_Types].xml"));
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn open(path: impl AsRef<Path>, limits: &PackageLimits) -> Result<Self, OpcError> {
        let path = path.as_ref();
        let file = open_source_file(path)?;
        let source = Arc::new(LazyArchive::new(file.try_clone()?));
        let mut package = Self::from_reader_internal(file, limits, Some(source))?;
        package.source_path = std::fs::canonicalize(path).ok();
        Ok(package)
    }

    /// Parse an OPC package from any seekable reader stream.
    ///
    /// All entry contents are read into memory (docx files are small; P1 does
    /// not stream). Reading is constrained by `limits`: entry counts and the
    /// per-entry and total uncompressed sizes are checked against ZIP metadata
    /// before any bytes are read; the compression ratio is checked after
    /// reading, to block zip bombs.
    ///
    /// # Failure cases
    ///
    /// - [`OpcError::LimitExceeded`] is returned when a limit was exceeded
    ///   (kind is `"entries"`, `"entry_uncompressed"`, `"total_uncompressed"`,
    ///   or `"compression_ratio"`)
    /// - [`OpcError::InvalidUri`] is returned when an entry name is invalid
    ///   (absolute path, `..`, backslash, etc.)
    /// - [`OpcError::DuplicateEntry`] is returned when entry names conflict
    ///   under any of the exact/case/percent scopes
    /// - [`OpcError::ZipRead`] is returned when ZIP data is corrupted or encrypted
    /// - [`OpcError::Malformed`] is returned when `[Content_Types].xml` or _rels/.rels is missing
    /// - [`OpcError::InvalidContentTypes`] / [`OpcError::InvalidRelationships`] are returned on XML parsing failures
    ///
    /// # Examples
    ///
    /// ```no_run
    /// use std::io::Cursor;
    ///
    /// use docxtpl_opc::{Package, PackageLimits};
    ///
    /// # let bytes: Vec<u8> = Vec::new(); // Actually the OPC package bytes
    /// let pkg = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    /// assert!(pkg.part_count() > 0);
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn from_reader<R: Read + Seek>(
        reader: R,
        limits: &PackageLimits,
    ) -> Result<Self, OpcError> {
        Self::from_reader_internal(reader, limits, None)
    }

    fn from_reader_internal<R: Read + Seek>(
        reader: R,
        limits: &PackageLimits,
        lazy_source: Option<Arc<LazyArchive>>,
    ) -> Result<Self, OpcError> {
        let mut reader = reader;

        // First scan the raw central directory: the zip crate folds duplicate
        // entry names via IndexMap while reading, so exact duplicates are only
        // visible when we parse the central directory ourselves. If the scan
        // fails, fall back to the zip crate's (duplicate-folded) view, in
        // which case only the case/percent scopes can be detected.
        let raw_names = match locate_central_directory(&mut reader) {
            Ok((entries, cd_start)) => {
                if entries > limits.max_entries as u64 {
                    return Err(OpcError::LimitExceeded {
                        kind: "entries",
                        value: entries,
                        max: limits.max_entries as u64,
                    });
                }
                read_central_directory_names(&mut reader, cd_start, entries).ok()
            }
            Err(_) => None,
        };

        let mut archive = ZipArchive::new(reader).map_err(zip_read_err)?;
        let count = archive.len();
        if raw_names.is_none() && count > limits.max_entries {
            return Err(OpcError::LimitExceeded {
                kind: "entries",
                value: count as u64,
                max: limits.max_entries as u64,
            });
        }

        // Three-scope duplicate detection (includes exact duplicates when the
        // scan succeeds; completed before any byte is read).
        let mut duplicates = DuplicateDetector::new();
        let check_in_loop = raw_names.is_none();
        if let Some(names) = &raw_names {
            for name in names {
                duplicates.check(name)?;
            }
        }

        let mut parts: Vec<Part> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut total_uncompressed: u64 = 0;

        for entry_number in 0..count {
            let mut entry = archive.by_index(entry_number).map_err(zip_read_err)?;
            let name = entry.name().to_string();
            let uri = PartUri::new(&name)?;

            if check_in_loop {
                duplicates.check(&name)?;
            }

            // Check metadata before reading bytes, to block zip bombs.
            let declared = entry.size();
            if declared > limits.max_entry_uncompressed {
                return Err(OpcError::LimitExceeded {
                    kind: "entry_uncompressed",
                    value: declared,
                    max: limits.max_entry_uncompressed,
                });
            }

            let is_dir = name.ends_with('/');
            let eager =
                lazy_source.is_none() || name == "[Content_Types].xml" || is_rels_path(&uri);
            let data = if is_dir {
                // Directory entry: keep it as an is_dir part with empty content.
                Vec::new()
            } else if eager {
                let remaining = limits
                    .max_total_uncompressed
                    .saturating_sub(total_uncompressed);
                let mut buf = Vec::new();
                entry
                    .by_ref()
                    .take(remaining.saturating_add(1))
                    .read_to_end(&mut buf)
                    .map_err(|err| OpcError::ZipRead {
                        detail: format!("entry {name:?}: {err}"),
                    })?;
                total_uncompressed = total_uncompressed.saturating_add(buf.len() as u64);
                if total_uncompressed > limits.max_total_uncompressed {
                    return Err(OpcError::LimitExceeded {
                        kind: "total_uncompressed",
                        value: total_uncompressed,
                        max: limits.max_total_uncompressed,
                    });
                }
                let compressed = entry.compressed_size();
                // For Stored entries compressed == uncompressed, so any ratio
                // limit is inherently satisfied; compressed == 0 (zero-length
                // entry) is also skipped.
                if compressed > 0 {
                    if let Some(allowed) = limits.max_compression_ratio.checked_mul(compressed) {
                        let actual = buf.len() as u64;
                        if actual > allowed {
                            let ratio = actual / compressed + u64::from(actual % compressed != 0);
                            return Err(OpcError::LimitExceeded {
                                kind: "compression_ratio",
                                value: ratio,
                                max: limits.max_compression_ratio,
                            });
                        }
                    }
                }
                buf
            } else {
                total_uncompressed = total_uncompressed.saturating_add(declared);
                if total_uncompressed > limits.max_total_uncompressed {
                    return Err(OpcError::LimitExceeded {
                        kind: "total_uncompressed",
                        value: total_uncompressed,
                        max: limits.max_total_uncompressed,
                    });
                }
                let compressed = entry.compressed_size();
                if compressed > 0 {
                    if let Some(allowed) = limits.max_compression_ratio.checked_mul(compressed) {
                        if declared > allowed {
                            let ratio =
                                declared / compressed + u64::from(declared % compressed != 0);
                            return Err(OpcError::LimitExceeded {
                                kind: "compression_ratio",
                                value: ratio,
                                max: limits.max_compression_ratio,
                            });
                        }
                    }
                }
                Vec::new()
            };

            let compression = entry.compression();
            let last_modified = entry.last_modified();
            index.insert(name, parts.len());
            if let Some(source) = lazy_source.as_ref().filter(|_| !eager && !is_dir) {
                parts.push(Part::new_lazy(
                    uri,
                    Arc::clone(source),
                    entry_number,
                    limits.max_entry_uncompressed,
                    is_dir,
                    compression,
                    last_modified,
                ));
            } else {
                let mut part = Part::new(uri, data, is_dir, compression, last_modified);
                if lazy_source.is_some() {
                    part.set_source_index(entry_number);
                }
                parts.push(part);
            }
        }

        let mut package = Package {
            limits: limits.clone(),
            source: lazy_source,
            source_path: None,
            parts,
            index,
            content_types: ContentTypes::default(),
            root_rels: Relationships::default(),
        };

        // [Content_Types].xml must exist.
        let ct_xml = package
            .part("[Content_Types].xml")
            .ok_or_else(|| OpcError::Malformed {
                reason: "missing [Content_Types].xml".to_string(),
            })?
            .bytes()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())?;
        package.content_types = ContentTypes::parse(&ct_xml)?;

        // _rels/.rels must exist.
        let root_xml = package
            .part("_rels/.rels")
            .ok_or_else(|| OpcError::Malformed {
                reason: "missing _rels/.rels".to_string(),
            })?
            .bytes()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())?;
        package.root_rels = Relationships::parse_in(&root_xml, "_rels/.rels")?;

        // Attach each <dir>/_rels/<name>.rels to the <dir>/<name> part in the
        // same directory.
        let mut attached: Vec<Option<Relationships>> = Vec::with_capacity(package.parts.len());
        for part in &package.parts {
            if part.is_dir() {
                attached.push(None);
                continue;
            }
            let rels_path = rels_path_for(part.uri());
            let Some(&rels_index) = package.index.get(&rels_path) else {
                attached.push(None);
                continue;
            };
            let xml = String::from_utf8_lossy(package.parts[rels_index].bytes()?).into_owned();
            attached.push(Some(Relationships::parse_in(&xml, &rels_path)?));
        }
        for (part, rels) in package.parts.iter_mut().zip(attached) {
            if let Some(rels) = rels {
                part.set_relationships(rels);
            }
        }

        Ok(package)
    }

    /// The limits used when opening.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert_eq!(pkg.limits().max_entries, 6_000);
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn limits(&self) -> &PackageLimits {
        &self.limits
    }

    /// Iterate over all parts in the original ZIP order (including directory
    /// entries).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// for part in pkg.parts() {
    ///     println!("{} ({} bytes)", part.name(), part.bytes()?.len());
    /// }
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn parts(&self) -> impl Iterator<Item = &Part> {
        self.parts.iter()
    }

    /// Total number of parts (including directory entries).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert_eq!(pkg.part_count(), pkg.parts().count());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn part_count(&self) -> usize {
        self.parts.len()
    }

    /// Find a part by exact entry name (the name must match the ZIP entry
    /// name).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let doc = pkg.part("word/document.xml").expect("main document exists");
    /// assert_eq!(doc.name(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.index.get(name).map(|&i| &self.parts[i])
    }

    /// Whether a part with the given entry name exists.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert!(pkg.contains("[Content_Types].xml"));
    /// assert!(!pkg.contains("word/missing.xml"));
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn contains(&self, name: &str) -> bool {
        self.index.contains_key(name)
    }

    /// Modify part contents and mark them as modified.
    ///
    /// If the target is `[Content_Types].xml` or a `.rels` file, the
    /// corresponding content types / relationships view is reparsed as well;
    /// if re-parsing fails, the whole call fails and the bytes stay unchanged.
    ///
    /// # Failure cases
    ///
    /// - [`OpcError::PartNotFound`] is returned when the entry does not exist
    /// - [`OpcError::InvalidContentTypes`] / [`OpcError::InvalidRelationships`] are returned when the new bytes are not valid XML of the corresponding kind
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let mut pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// pkg.set_part_bytes("word/document.xml", b"<w:document/>".to_vec())?;
    /// assert_eq!(pkg.part("word/document.xml").expect("main document exists").bytes()?, b"<w:document/>");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn set_part_bytes(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), OpcError> {
        let Some(&index) = self.index.get(name) else {
            return Err(OpcError::PartNotFound {
                uri: name.to_string(),
            });
        };
        let xml = String::from_utf8_lossy(&bytes).into_owned();
        if name == "[Content_Types].xml" {
            self.content_types = ContentTypes::parse(&xml)?;
        } else if name == "_rels/.rels" {
            self.root_rels = Relationships::parse_in(&xml, name)?;
        } else if let Some(owner) = owner_of_rels_path(self.parts[index].uri()) {
            if let Some(&owner_index) = self.index.get(&owner) {
                let rels = Relationships::parse_in(&xml, name)?;
                self.parts[owner_index].set_relationships(rels);
            }
        }
        self.parts[index].replace_bytes(bytes);
        Ok(())
    }

    /// Rebuild the content types view from the package's current parts,
    /// following python-docx `_ContentTypesItem.from_parts` (0.20.2).
    ///
    /// Enumerates every part except `[Content_Types].xml`, `.rels`, and
    /// directory entries, and reassigns Default/Override ownership according
    /// to each part's current content type (in real Word templates the Override
    /// entries for rels/customXml disappear here; the rels/xml Default entries
    /// are always present). This method only changes the in-memory view; the
    /// caller then takes `to_xml()` of [`Package::content_types`] and persists
    /// it via [`Package::set_part_bytes`].
    pub fn rebuild_content_types(&mut self) {
        let entries: Vec<(String, String)> = self
            .parts
            .iter()
            .filter(|part| {
                !part.is_dir() && part.name() != "[Content_Types].xml" && !is_rels_path(part.uri())
            })
            .filter_map(|part| {
                self.content_types
                    .content_type_of(part.uri())
                    .map(|ct| (part.name().to_string(), ct.to_string()))
            })
            .collect();
        self.content_types
            .rebuild_from_parts(entries.iter().map(|(a, b)| (a.as_str(), b.as_str())));
    }

    /// Rewrite every `.rels` in the package (the root rels and the rels of each
    /// attached part) to the model's canonical serialization (lxml
    /// single-quoted declaration), matching python-docx `PackageWriter`, which
    /// **always rewrites** rels streams on save (the double-quoted
    /// declarations and trailing newlines of real Word templates are
    /// normalized). Bytes identical to the current entry are not written back
    /// and do not mark the part as modified.
    ///
    /// # Failure cases
    ///
    /// The canonical XML must always pass the internal
    /// `Relationships::parse_in` (the model's own output is always parsable);
    /// write-back failures can only come from [`Package::set_part_bytes`].
    pub fn normalize_relationships(&mut self) -> Result<(), OpcError> {
        let root_xml = self.root_rels.to_xml();
        let root_changed = match self.part("_rels/.rels") {
            Some(part) => part.bytes()? != root_xml.as_bytes(),
            None => true,
        };
        if root_changed {
            self.set_part_bytes("_rels/.rels", root_xml.into_bytes())?;
        }
        // Collect (rels path, canonical bytes) before writing back, to avoid
        // aliasing &mut while iterating.
        let pending: Vec<(String, Vec<u8>)> = self
            .parts
            .iter()
            .filter_map(|part| {
                let rels = part.relationships()?;
                let rels_path = rels_path_for(part.uri());
                Some((rels_path, rels.to_xml().into_bytes()))
            })
            .collect();
        for (rels_path, bytes) in pending {
            let changed = match self.part(&rels_path) {
                Some(part) => part.bytes()? != bytes.as_slice(),
                None => true,
            };
            if changed {
                self.set_part_bytes(&rels_path, bytes)?;
            }
        }
        Ok(())
    }

    /// Append a new part (e.g. a `word/media/imageN.*` injected by rendering).
    ///
    /// The new part is written with Deflate compression and the default ZIP
    /// timestamp (matching how python-docx `PackageWriter` writes new image
    /// parts); the entry is appended at the end of parts.
    ///
    /// # Failure cases
    ///
    /// - [`OpcError::InvalidUri`] is returned when the entry name is invalid
    /// - [`OpcError::DuplicateEntry`] is returned when the entry already exists
    ///   (exact match; new paths are assigned by the caller by image number, so
    ///   case/percent variants do not arise in construction scenarios)
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let mut pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// pkg.add_part("word/media/image1.png", b"\x89PNG".to_vec())?;
    /// assert!(pkg.contains("word/media/image1.png"));
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn add_part(&mut self, name: &str, bytes: Vec<u8>) -> Result<(), OpcError> {
        let uri = PartUri::new(name)?;
        if self.index.contains_key(name) {
            return Err(OpcError::DuplicateEntry {
                uri: name.to_string(),
                scope: "exact",
            });
        }
        self.index.insert(name.to_string(), self.parts.len());
        self.parts.push(Part::new(
            uri,
            bytes,
            false,
            CompressionMethod::Deflated,
            None,
        ));
        Ok(())
    }

    /// Appends a file-backed part whose content is streamed during ZIP writing.
    ///
    /// The source metadata and SHA-1 digest are verified during serialization;
    /// changing the file before or during the write returns an error.
    pub fn add_file_backed_part(
        &mut self,
        name: &str,
        path: impl Into<PathBuf>,
        len: u64,
        modified: Option<std::time::SystemTime>,
        digest: [u8; 20],
    ) -> Result<(), OpcError> {
        let uri = PartUri::new(name)?;
        if self.index.contains_key(name) {
            return Err(OpcError::DuplicateEntry {
                uri: name.to_string(),
                scope: "exact",
            });
        }
        self.index.insert(name.to_string(), self.parts.len());
        self.parts.push(Part::new_file_backed(
            uri,
            path.into(),
            len,
            modified,
            digest,
        ));
        Ok(())
    }

    /// The parsed view of `[Content_Types].xml`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let uri = pkg.part("word/document.xml").expect("main document exists").uri().clone();
    /// assert!(pkg.content_types().content_type_of(&uri).is_some());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn content_types(&self) -> &ContentTypes {
        &self.content_types
    }

    /// The root relationships (`_rels/.rels`).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert!(!pkg.root_relationships().is_empty());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn root_relationships(&self) -> &Relationships {
        &self.root_rels
    }

    /// The relationship set of a given part (equivalent to
    /// `part(name)?.relationships()`).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let rels = pkg.relationships_of("word/document.xml");
    /// assert_eq!(rels.is_some(), pkg.contains("word/_rels/document.xml.rels"));
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn relationships_of(&self, part: &str) -> Option<&Relationships> {
        self.part(part).and_then(Part::relationships)
    }

    /// The main document part URI: the target of the internal relationship
    /// whose rel_type is officeDocument in the root relationships
    /// (resolved relative to the package root).
    ///
    /// # Failure cases
    ///
    /// [`OpcError::Malformed`] is returned when there is no internal
    /// officeDocument relationship, or its target cannot be resolved to a
    /// package-internal URI. Whether the target part exists is checked by
    /// [`Package::validate`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert_eq!(pkg.main_document_uri()?.as_str(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn main_document_uri(&self) -> Result<PartUri, OpcError> {
        for rel in self.root_rels.iter() {
            if rel.rel_type == OFFICE_DOCUMENT_REL_TYPE && rel.target_mode == TargetMode::Internal {
                return resolve_relative_to(None, &rel.target).ok_or_else(|| OpcError::Malformed {
                    reason: format!(
                        "officeDocument relationship target {:?} cannot be resolved to a package-internal URI",
                        rel.target
                    ),
                });
            }
        }
        Err(OpcError::Malformed {
            reason: "root relationships are missing an officeDocument relationship".to_string(),
        })
    }

    /// Validate package integrity:
    ///
    /// 1. `[Content_Types].xml`, `_rels/.rels`, and the main document target
    ///    must exist;
    /// 2. every internal relationship must resolve to an existing part
    ///    (a dangling one is an error);
    /// 3. Ids must be unique within each rels file;
    /// 4. every part other than `[Content_Types].xml`, `_rels/*`, and
    ///    directory entries must have a content type.
    ///
    /// # Failure cases
    ///
    /// - [`OpcError::MissingPart`] is returned when a required part is missing
    /// - [`OpcError::Malformed`] is returned when root relationships are missing officeDocument
    /// - [`OpcError::InvalidRelationships`] is returned for a dangling internal relationship or a duplicate Id
    /// - [`OpcError::InvalidContentTypes`] is returned when a regular part has no content type
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// pkg.validate()?;
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn validate(&self) -> Result<(), OpcError> {
        for required in ["[Content_Types].xml", "_rels/.rels"] {
            if !self.contains(required) {
                return Err(OpcError::MissingPart {
                    uri: required.to_string(),
                });
            }
        }
        let main = self.main_document_uri()?;
        if !self.contains(main.as_str()) {
            return Err(OpcError::MissingPart {
                uri: main.as_str().to_string(),
            });
        }
        self.check_rels_targets(None, &self.root_rels, "_rels/.rels")?;
        for part in &self.parts {
            if let Some(rels) = part.relationships() {
                let rels_path = rels_path_for(part.uri());
                // Same as Relationships::resolve: resolve the target relative
                // to the part's directory
                let base = part.uri().parent();
                self.check_rels_targets(base.as_ref(), rels, &rels_path)?;
            }
        }
        self.check_rels_ids(&self.root_rels, "_rels/.rels")?;
        for part in &self.parts {
            if let Some(rels) = part.relationships() {
                let rels_path = rels_path_for(part.uri());
                self.check_rels_ids(rels, &rels_path)?;
            }
        }
        for part in &self.parts {
            if part.is_dir() || part.name() == "[Content_Types].xml" || is_rels_path(part.uri()) {
                continue;
            }
            if self.content_types.content_type_of(part.uri()).is_none() {
                return Err(OpcError::InvalidContentTypes {
                    reason: format!("part {:?} has no matching content type", part.name()),
                });
            }
        }
        Ok(())
    }

    /// Save to a file.
    ///
    /// For a new path, the ZIP is first streamed to a temporary file in the
    /// same directory and then atomically persisted; an existing path is
    /// truncated and written directly, to stay compatible with a target handle
    /// on Windows that the caller still holds open. See
    /// [`Package::write_to`] for write details.
    ///
    /// # Failure cases
    ///
    /// [`OpcError::LimitExceeded`] (kind `"output"`), [`OpcError::ZipWrite`],
    /// or [`OpcError::Io`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// pkg.save("out.docx")?;
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), OpcError> {
        self.save_with_options(path, &WriteOptions::compatible())
    }

    /// Save to a file using explicit ZIP serialization options.
    pub fn save_with_options(
        &self,
        path: impl AsRef<Path>,
        options: &WriteOptions,
    ) -> Result<(), OpcError> {
        let path = path.as_ref();
        let overwrites_source = self
            .source_path
            .as_ref()
            .zip(std::fs::canonicalize(path).ok().as_ref())
            .is_some_and(|(source, target)| source == target);
        if overwrites_source {
            // Windows cannot reliably use rename to overwrite a source file
            // that is still open. Materialize all parts first, then close the
            // lazy backend and write with truncation; only same-path saves pay
            // this memory cost.
            for part in &self.parts {
                part.bytes()?;
            }
            if let Some(source) = &self.source {
                source.close()?;
            }
            return self.write_to_with_options(std::fs::File::create(path)?, options);
        }
        if path.exists() && !overwrites_source {
            // On Windows the target may still be held open by the caller (a
            // NamedTempFile, for example), so rename/persist cannot replace it;
            // a direct truncated write still keeps the streaming memory
            // profile. Brand-new file paths go through the same-directory
            // temporary file below so a failure never leaves a truncated file.
            return self.write_to_with_options(std::fs::File::create(path)?, options);
        }
        let parent = path.parent().filter(|value| !value.as_os_str().is_empty());
        let mut temporary = match parent {
            Some(parent) => tempfile::NamedTempFile::new_in(parent)?,
            None => tempfile::NamedTempFile::new_in(".")?,
        };
        // Stream the ZIP directly to the target file to avoid a second
        // in-memory copy of `max_output_size` scale. The temporary file lives
        // in the same directory and is persisted only on success, so a write
        // failure never leaves a truncated target document.
        self.write_to_with_options(temporary.as_file_mut(), options)?;
        temporary
            .persist(path)
            .map_err(|error| OpcError::Io(error.error))?;
        Ok(())
    }

    /// Write to any seekable writer stream.
    ///
    /// Entries are written one by one in the original [`Package::parts`]
    /// order. Entries opened from a path and left unmodified are copied
    /// directly from the source ZIP's compressed data, without decompressing
    /// or recompressing; modified entries are written from the new bytes.
    /// Entry names are kept as-is; newly written entries keep the original
    /// compression method (Stored→Stored, everything else→Deflated at the
    /// default level) and any available timestamp; directory entries are
    /// written as zero-length Stored.
    ///
    /// # Failure cases
    ///
    /// - [`OpcError::LimitExceeded`] is returned when the total output exceeds
    ///   `max_output_size` (kind `"output"`; raised as soon as the counting
    ///   writer fills up)
    /// - [`OpcError::ZipWrite`] is returned on an underlying write failure
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use std::io::Cursor;
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let mut out = Vec::new();
    /// pkg.write_to(Cursor::new(&mut out))?;
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn write_to(&self, mut writer: impl Write + Seek) -> Result<(), OpcError> {
        self.write_to_with_options(&mut writer, &WriteOptions::compatible())
    }

    /// Write to any seekable writer using explicit ZIP serialization options.
    pub fn write_to_with_options(
        &self,
        mut writer: impl Write + Seek,
        options: &WriteOptions,
    ) -> Result<(), OpcError> {
        let max = self.limits.max_output_size;
        let mut limited = CountingWriter::new(&mut writer, max);
        let result = write_zip(&self.parts, self.source.as_deref(), &mut limited, options);
        let exceeded = limited.exceeded();
        let attempted = limited.attempted();
        match result {
            Ok(()) => {
                limited.flush()?;
                Ok(())
            }
            Err(err) => {
                if exceeded {
                    Err(OpcError::LimitExceeded {
                        kind: "output",
                        value: attempted,
                        max,
                    })
                } else {
                    Err(err)
                }
            }
        }
    }

    /// Validate that the internal targets of a set of relationships all hit
    /// parts that exist in the package.
    fn check_rels_targets(
        &self,
        base: Option<&PartUri>,
        rels: &Relationships,
        source: &str,
    ) -> Result<(), OpcError> {
        for rel in rels.iter() {
            if rel.target_mode != TargetMode::Internal {
                continue;
            }
            let hit = resolve_relative_to(base, &rel.target)
                .is_some_and(|uri| self.contains(uri.as_str()));
            if !hit {
                return Err(OpcError::InvalidRelationships {
                    reason: format!(
                        "{source}: relationship {} target {:?} does not match any part in the package",
                        rel.id, rel.target
                    ),
                });
            }
        }
        Ok(())
    }

    /// Validate that Ids are unique within a set of relationships.
    fn check_rels_ids(&self, rels: &Relationships, source: &str) -> Result<(), OpcError> {
        let mut seen: HashSet<&str> = HashSet::new();
        for rel in rels.iter() {
            if !seen.insert(rel.id.as_str()) {
                return Err(OpcError::InvalidRelationships {
                    reason: format!("{source}: duplicate relationship Id {}", rel.id),
                });
            }
        }
        Ok(())
    }
}

impl std::fmt::Debug for Package {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Package")
            .field("limits", &self.limits)
            .field("part_count", &self.part_count())
            .field("root_rels", &self.root_rels)
            .finish()
    }
}

/// Counting writer wrapper: counts output bytes by the stream's logical
/// position (seek syncs the position, so repeated rewrites are not counted
/// twice) and errors as soon as the count would exceed `max` (stops when full,
/// leaving no truncated file).
struct CountingWriter<W> {
    inner: W,
    pos: u64,
    max: u64,
    exceeded: bool,
    attempted: u64,
}

impl<W> CountingWriter<W> {
    fn new(inner: W, max: u64) -> Self {
        Self {
            inner,
            pos: 0,
            max,
            exceeded: false,
            attempted: 0,
        }
    }

    fn exceeded(&self) -> bool {
        self.exceeded
    }

    fn attempted(&self) -> u64 {
        self.attempted
    }
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let end = self.pos.saturating_add(buf.len() as u64);
        if end > self.max {
            self.exceeded = true;
            if end > self.attempted {
                self.attempted = end;
            }
            return Err(std::io::Error::new(
                std::io::ErrorKind::WriteZero,
                "output exceeds the max_output_size limit",
            ));
        }
        let written = self.inner.write(buf)?;
        self.pos = self.pos.saturating_add(written as u64);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl<W: Seek> Seek for CountingWriter<W> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let position = self.inner.seek(pos)?;
        self.pos = position;
        Ok(position)
    }
}

#[cfg(windows)]
fn open_source_file(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE: lets a same-path
    // streaming save replace the directory entry with a temporary file while
    // the old ZIP is still being read.
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x0000_0001 | 0x0000_0002 | 0x0000_0004)
        .open(path)
}

#[cfg(not(windows))]
fn open_source_file(path: &Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

/// Write out every entry in the original parts order (see the behavior notes
/// on [`Package::write_to`]).
fn write_zip<W: Write + Seek>(
    parts: &[Part],
    source: Option<&LazyArchive>,
    out: &mut CountingWriter<W>,
    options: &WriteOptions,
) -> Result<(), OpcError> {
    if let Some(source) = source {
        let mut source_file = source.lock()?;
        if let Some(file) = source_file.as_mut() {
            let mut archive = ZipArchive::new(file).map_err(zip_read_err)?;
            return write_zip_entries(parts, Some(&mut archive), out, options);
        }
    }
    write_zip_entries::<W, std::io::Cursor<Vec<u8>>>(parts, None, out, options)
}

fn write_zip_entries<W: Write + Seek, R: Read + Seek>(
    parts: &[Part],
    mut source: Option<&mut ZipArchive<R>>,
    out: &mut CountingWriter<W>,
    write_options: &WriteOptions,
) -> Result<(), OpcError> {
    let mut zip = ZipWriter::new(out);
    for part in parts {
        if !part.is_modified() {
            if let (Some(archive), Some(index)) = (source.as_deref_mut(), part.source_index()) {
                let entry = archive.by_index_raw(index).map_err(zip_read_err)?;
                if entry.name() != part.name() {
                    return Err(OpcError::ZipRead {
                        detail: format!(
                            "source ZIP changed: index {index} expected {:?}, got {:?}",
                            part.name(),
                            entry.name()
                        ),
                    });
                }
                zip.raw_copy_file(entry)
                    .map_err(|error| OpcError::ZipWrite {
                        detail: error.to_string(),
                    })?;
                continue;
            }
        }
        let media = is_media_part(part.name());
        let (method, level) = if part.is_dir() {
            (CompressionMethod::Stored, None)
        } else if media {
            match write_options.media_compression {
                MediaCompression::Compatible => {
                    if part.compression_method() == CompressionMethod::Stored {
                        (CompressionMethod::Stored, None)
                    } else {
                        (CompressionMethod::Deflated, None)
                    }
                }
                MediaCompression::FastDeflate => (CompressionMethod::Deflated, Some(1)),
                MediaCompression::Stored => (CompressionMethod::Stored, None),
                MediaCompression::Auto => auto_media_compression(part.name()),
            }
        } else if part.compression_method() == CompressionMethod::Stored {
            (CompressionMethod::Stored, None)
        } else {
            (CompressionMethod::Deflated, None)
        };
        let mut options = SimpleFileOptions::default()
            .compression_method(method)
            .compression_level(level);
        if let Some(time) = part.last_modified() {
            options = options.last_modified_time(time);
        }
        zip.start_file(part.name(), options)
            .map_err(|error| OpcError::ZipWrite {
                detail: error.to_string(),
            })?;
        if !part.is_dir() {
            part.write_content(&mut zip)?;
        }
    }
    zip.finish().map_err(|error| OpcError::ZipWrite {
        detail: error.to_string(),
    })?;
    Ok(())
}

fn is_media_part(name: &str) -> bool {
    name.strip_prefix('/')
        .is_some_and(|name| name.starts_with("word/media/"))
        || name.starts_with("word/media/")
}

fn auto_media_compression(name: &str) -> (CompressionMethod, Option<i64>) {
    let extension = name.rsplit_once('.').map(|(_, extension)| extension);
    if extension.is_some_and(|extension| {
        matches!(
            extension.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "gif" | "tif" | "tiff"
        )
    }) {
        (CompressionMethod::Stored, None)
    } else {
        (CompressionMethod::Deflated, Some(1))
    }
}

/// Three-scope duplicate entry detector: exact (with empty-segment
/// collapse), case folding, and percent-decode folding (decode first, then
/// collapse). Stored entry names are kept unchanged; this is used for
/// detection only.
struct DuplicateDetector {
    seen_exact: HashSet<String>,
    seen_case: HashSet<String>,
    seen_percent: HashSet<String>,
}

impl DuplicateDetector {
    fn new() -> Self {
        Self {
            seen_exact: HashSet::new(),
            seen_case: HashSet::new(),
            seen_percent: HashSet::new(),
        }
    }

    /// Record an entry name; return [`OpcError::DuplicateEntry`] on a conflict
    /// under any scope.
    fn check(&mut self, name: &str) -> Result<(), OpcError> {
        let key_exact = collapse_segments(name);
        let key_case = key_exact.to_lowercase();
        let decoded = String::from_utf8_lossy(&percent_decode(name)).into_owned();
        let key_percent = collapse_segments(&decoded).to_lowercase();
        if !self.seen_exact.insert(key_exact) {
            return Err(OpcError::DuplicateEntry {
                uri: name.to_string(),
                scope: "exact",
            });
        }
        if !self.seen_case.insert(key_case) {
            return Err(OpcError::DuplicateEntry {
                uri: name.to_string(),
                scope: "case",
            });
        }
        if !self.seen_percent.insert(key_percent) {
            return Err(OpcError::DuplicateEntry {
                uri: name.to_string(),
                scope: "percent",
            });
        }
        Ok(())
    }
}

/// Locate the ZIP's EOCD (including zip64 sentinel handling); returns
/// (the raw entry count declared by the central directory, the actual start
/// position of the central directory).
///
/// The actual position is derived from `EOCD position - cd_size`, so this also
/// works for ZIPs with prepended data (e.g. a self-extracting header). Returns
/// Err when it cannot be parsed reliably, and the caller falls back to the zip
/// crate's view.
fn locate_central_directory<R: Read + Seek>(reader: &mut R) -> Result<(u64, u64), String> {
    const EOCD_LEN: usize = 22;
    const MAX_COMMENT: u64 = 65_535;
    const SIG_EOCD: u32 = 0x0605_4B50;
    const SIG_EOCD64: u32 = 0x0606_4B50;
    const SIG_LOCATOR64: u32 = 0x0706_4B50;

    fn u16_at(buf: &[u8], at: usize) -> u16 {
        u16::from_le_bytes([buf[at], buf[at + 1]])
    }
    fn u32_at(buf: &[u8], at: usize) -> u32 {
        u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
    }

    let file_len = reader
        .seek(SeekFrom::End(0))
        .map_err(|err| err.to_string())?;
    if (file_len as usize) < EOCD_LEN {
        return Err("file is shorter than the EOCD".to_string());
    }
    let scan_start = file_len.saturating_sub(EOCD_LEN as u64 + MAX_COMMENT);
    let scan_len = (file_len - scan_start) as usize;
    reader
        .seek(SeekFrom::Start(scan_start))
        .map_err(|err| err.to_string())?;
    let mut tail = vec![0u8; scan_len];
    reader
        .read_exact(&mut tail)
        .map_err(|err| err.to_string())?;

    // Search backwards for an EOCD whose signature and comment length agree.
    let mut eocd = None;
    let mut at = tail.len() - EOCD_LEN;
    loop {
        if u32_at(&tail, at) == SIG_EOCD {
            let comment_len = u16_at(&tail, at + 20) as usize;
            if at + EOCD_LEN + comment_len == tail.len() {
                eocd = Some(at);
                break;
            }
        }
        if at == 0 {
            break;
        }
        at -= 1;
    }
    let eocd = eocd.ok_or_else(|| "EOCD not found".to_string())?;

    let mut entries = u64::from(u16_at(&tail, eocd + 10));
    let cd_size = u64::from(u32_at(&tail, eocd + 12));

    // zip64 sentinel: the EOCD64 locator usually sits immediately (20 bytes)
    // before the EOCD.
    let needs_zip64 =
        entries == 0xFFFF || cd_size == 0xFFFF_FFFF || u32_at(&tail, eocd + 16) == 0xFFFF_FFFF;
    if needs_zip64 && eocd >= 20 && u32_at(&tail, eocd - 20) == SIG_LOCATOR64 {
        let locator = eocd - 20;
        let eocd64_offset = u64::from_le_bytes([
            tail[locator + 8],
            tail[locator + 9],
            tail[locator + 10],
            tail[locator + 11],
            tail[locator + 12],
            tail[locator + 13],
            tail[locator + 14],
            tail[locator + 15],
        ]);
        let mut eocd64 = [0u8; 56];
        let read = reader
            .seek(SeekFrom::Start(eocd64_offset))
            .and_then(|_| reader.read_exact(&mut eocd64));
        if read.is_ok() && u32_at(&eocd64, 0) == SIG_EOCD64 {
            entries = u64::from_le_bytes([
                eocd64[32], eocd64[33], eocd64[34], eocd64[35], eocd64[36], eocd64[37], eocd64[38],
                eocd64[39],
            ]);
        }
    }

    // Actual central directory position = actual EOCD position - central
    // directory size (accommodates prepended data).
    let absolute_eocd = scan_start + eocd as u64;
    let cd_start = absolute_eocd
        .checked_sub(cd_size)
        .ok_or_else(|| "EOCD central directory size contradicts its position".to_string())?;
    Ok((entries, cd_start))
}

/// Read all entry names from the central directory sequentially (without
/// deduplication). Names use lossy UTF-8 and are only for duplicate
/// detection; the entry count has already passed the max_entries check, so
/// memory use is bounded.
fn read_central_directory_names<R: Read + Seek>(
    reader: &mut R,
    cd_start: u64,
    entries: u64,
) -> Result<Vec<String>, String> {
    const SIG_CDH: u32 = 0x0201_4B50;

    reader
        .seek(SeekFrom::Start(cd_start))
        .map_err(|err| err.to_string())?;
    let mut names = Vec::new();
    for _ in 0..entries {
        let mut header = [0u8; 46];
        reader
            .read_exact(&mut header)
            .map_err(|err| err.to_string())?;
        if u32::from_le_bytes([header[0], header[1], header[2], header[3]]) != SIG_CDH {
            return Err("bad central directory record signature".to_string());
        }
        let name_len = u16::from_le_bytes([header[28], header[29]]) as usize;
        let extra_len = u16::from_le_bytes([header[30], header[31]]) as usize;
        let comment_len = u16::from_le_bytes([header[32], header[33]]) as usize;
        let mut name_bytes = vec![0u8; name_len];
        reader
            .read_exact(&mut name_bytes)
            .map_err(|err| err.to_string())?;
        reader
            .seek(SeekFrom::Current((extra_len + comment_len) as i64))
            .map_err(|err| err.to_string())?;
        names.push(String::from_utf8_lossy(&name_bytes).into_owned());
    }
    Ok(names)
}

fn zip_read_err(err: zip::result::ZipError) -> OpcError {
    OpcError::ZipRead {
        detail: err.to_string(),
    }
}

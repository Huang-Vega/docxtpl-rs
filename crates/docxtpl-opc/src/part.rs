//! A single part inside a package.

use std::fmt;
use std::fs::File;
use std::io::Read;
use std::sync::{Arc, Mutex, OnceLock};

use zip::CompressionMethod;
use zip::DateTime;

use crate::rels::Relationships;
use crate::uri::PartUri;
use crate::OpcError;

pub(crate) struct LazyArchive {
    file: Mutex<Option<File>>,
}

impl LazyArchive {
    pub(crate) fn new(file: File) -> Self {
        Self {
            file: Mutex::new(Some(file)),
        }
    }

    pub(crate) fn lock(&self) -> Result<std::sync::MutexGuard<'_, Option<File>>, OpcError> {
        self.file.lock().map_err(|_| OpcError::ZipRead {
            detail: "source ZIP file lock is poisoned".to_string(),
        })
    }

    pub(crate) fn close(&self) -> Result<(), OpcError> {
        self.lock()?.take();
        Ok(())
    }
}

enum PartData {
    Loaded(Vec<u8>),
    Lazy {
        source: Arc<LazyArchive>,
        entry_index: usize,
        max_bytes: u64,
        cache: OnceLock<Result<Vec<u8>, String>>,
    },
}

/// A single part (including directory entries: `is_dir() == true` with empty
/// `bytes()`).
///
/// Obtain one via [`crate::Package::parts`] / [`crate::Package::part`];
/// to modify its contents use [`crate::Package::set_part_bytes`].
///
/// # Examples
///
/// ```no_run
/// use docxtpl_opc::{Package, PackageLimits};
///
/// let pkg = Package::open("template.docx", &PackageLimits::default())?;
/// let doc = pkg.part("word/document.xml").expect("main document exists");
/// assert_eq!(doc.name(), "word/document.xml");
/// assert!(!doc.is_modified());
/// # Ok::<(), docxtpl_opc::OpcError>(())
/// ```
pub struct Part {
    uri: PartUri,
    data: PartData,
    source_index: Option<usize>,
    modified: bool,
    dir: bool,
    relationships: Option<Relationships>,
    compression: CompressionMethod,
    last_modified: Option<DateTime>,
}

impl Part {
    pub(crate) fn new(
        uri: PartUri,
        data: Vec<u8>,
        dir: bool,
        compression: CompressionMethod,
        last_modified: Option<DateTime>,
    ) -> Self {
        Self {
            uri,
            data: PartData::Loaded(data),
            source_index: None,
            modified: false,
            dir,
            relationships: None,
            compression,
            last_modified,
        }
    }

    pub(crate) fn new_lazy(
        uri: PartUri,
        source: Arc<LazyArchive>,
        entry_index: usize,
        max_bytes: u64,
        dir: bool,
        compression: CompressionMethod,
        last_modified: Option<DateTime>,
    ) -> Self {
        Self {
            uri,
            data: if dir {
                PartData::Loaded(Vec::new())
            } else {
                PartData::Lazy {
                    source,
                    entry_index,
                    max_bytes,
                    cache: OnceLock::new(),
                }
            },
            source_index: Some(entry_index),
            modified: false,
            dir,
            relationships: None,
            compression,
            last_modified,
        }
    }

    pub(crate) fn set_relationships(&mut self, rels: Relationships) {
        self.relationships = Some(rels);
    }

    pub(crate) fn replace_bytes(&mut self, data: Vec<u8>) {
        self.data = PartData::Loaded(data);
        self.modified = true;
    }

    pub(crate) fn compression_method(&self) -> CompressionMethod {
        self.compression
    }

    pub(crate) fn last_modified(&self) -> Option<DateTime> {
        self.last_modified
    }

    pub(crate) fn source_index(&self) -> Option<usize> {
        self.source_index
    }

    pub(crate) fn set_source_index(&mut self, index: usize) {
        self.source_index = Some(index);
    }

    /// The part URI.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let doc = pkg.part("word/document.xml").expect("main document exists");
    /// assert_eq!(doc.uri().as_str(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn uri(&self) -> &PartUri {
        &self.uri
    }

    /// The entry name (i.e. `uri().as_str()`).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert_eq!(pkg.part("word/document.xml").expect("main document exists").name(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn name(&self) -> &str {
        self.uri.as_str()
    }

    /// The current content bytes (the new bytes after modification).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let doc = pkg.part("word/document.xml").expect("main document exists");
    /// let content: &[u8] = doc.bytes()?;
    /// assert!(!content.is_empty());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn bytes(&self) -> Result<&[u8], OpcError> {
        match &self.data {
            PartData::Loaded(data) => Ok(data),
            PartData::Lazy {
                source,
                entry_index,
                max_bytes,
                cache,
            } => match cache.get_or_init(|| {
                let mut source_file = source.lock().map_err(|error| error.to_string())?;
                let file = source_file.as_mut().ok_or_else(|| {
                    "source ZIP is closed and the part has not been materialized".to_string()
                })?;
                let mut archive = zip::ZipArchive::new(file).map_err(|error| error.to_string())?;
                let mut entry = archive
                    .by_index(*entry_index)
                    .map_err(|error| error.to_string())?;
                if entry.name() != self.name() {
                    return Err(format!(
                        "source ZIP changed: index {entry_index} is now {:?}",
                        entry.name()
                    ));
                }
                let mut data = Vec::new();
                entry
                    .by_ref()
                    .take(max_bytes.saturating_add(1))
                    .read_to_end(&mut data)
                    .map_err(|error| error.to_string())?;
                if data.len() as u64 > *max_bytes {
                    return Err(format!(
                        "uncompressed entry exceeds the {max_bytes}-byte limit"
                    ));
                }
                Ok(data)
            }) {
                Ok(data) => Ok(data),
                Err(detail) => Err(OpcError::ZipRead {
                    detail: format!("entry {:?}: {detail}", self.name()),
                }),
            },
        }
    }

    /// Whether the content is already resident in memory.
    ///
    /// Packages opened from a path lazily read regular parts; this returns
    /// `true` after the first call to [`Self::bytes`]. Packages opened from a
    /// generic reader, and newly created or modified parts, always return
    /// `true`.
    pub fn is_loaded(&self) -> bool {
        match &self.data {
            PartData::Loaded(_) => true,
            PartData::Lazy { cache, .. } => cache.get().is_some(),
        }
    }

    /// The relationship set of this part; `None` when there is no corresponding
    /// `_rels/<name>.rels`.
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// // Depends on whether word/_rels/document.xml.rels exists
    /// let rels = pkg.part("word/document.xml").expect("main document exists").relationships();
    /// assert_eq!(rels.is_some(), pkg.contains("word/_rels/document.xml.rels"));
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn relationships(&self) -> Option<&Relationships> {
        self.relationships.as_ref()
    }

    /// Whether this is a directory entry (an entry name ending with `/`).
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let dir = pkg.part("word/");
    /// let file = pkg.part("word/document.xml");
    /// assert!(dir.map(|p| p.is_dir()).unwrap_or(false));
    /// assert!(file.map(|p| !p.is_dir()).unwrap_or(false));
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn is_dir(&self) -> bool {
        self.dir
    }

    /// Whether the content was modified by
    /// [`crate::Package::set_part_bytes`].
    ///
    /// # Examples
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let mut pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert!(!pkg.part("word/document.xml").expect("main document exists").is_modified());
    /// pkg.set_part_bytes("word/document.xml", b"<w:document/>".to_vec())?;
    /// assert!(pkg.part("word/document.xml").expect("main document exists").is_modified());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn is_modified(&self) -> bool {
        self.modified
    }
}

impl fmt::Debug for Part {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Part")
            .field("uri", &self.uri)
            .field(
                "loaded_len",
                &match &self.data {
                    PartData::Loaded(data) => Some(data.len()),
                    PartData::Lazy { cache, .. } => cache
                        .get()
                        .and_then(|result| result.as_ref().ok())
                        .map(Vec::len),
                },
            )
            .field("modified", &self.modified)
            .field("dir", &self.dir)
            .field(
                "relationships",
                &self.relationships.as_ref().map(Relationships::len),
            )
            .finish()
    }
}

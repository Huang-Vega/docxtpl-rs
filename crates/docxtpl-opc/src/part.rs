//! A single part inside a package.

use std::fmt;
use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

use sha1::{Digest, Sha1};
use zip::CompressionMethod;
use zip::DateTime;

use crate::rels::Relationships;
use crate::uri::PartUri;
use crate::OpcError;

/// An immutable snapshot of a file used as an OPC part source.
///
/// The snapshot records the file length, modification time, and SHA-1 digest.
/// [`crate::Package`] verifies all three again while serializing the part, so
/// replacing or truncating the source after this snapshot was created fails
/// instead of silently producing a different package.
#[derive(Debug, Clone)]
pub struct FilePartSource {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    digest: [u8; 20],
}

impl FilePartSource {
    /// Snapshot a regular file without retaining its contents in memory.
    ///
    /// # Failure cases
    ///
    /// Returns [`OpcError::Io`] when the path cannot be read, is not a regular
    /// file, or changes while its digest is being computed.
    pub fn snapshot(path: impl AsRef<Path>) -> Result<Self, OpcError> {
        let path = path.as_ref();
        let before = std::fs::metadata(path)?;
        if !before.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("file-backed part source is not a file: {}", path.display()),
            )
            .into());
        }
        let len = before.len();
        let modified = before.modified().ok();
        let mut file = File::open(path)?;
        let mut hasher = Sha1::new();
        let mut read_len = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            read_len = read_len.saturating_add(read as u64);
            hasher.update(&buffer[..read]);
        }
        validate_file_metadata(path, len, modified)?;
        if read_len != len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("file-backed part source changed: {}", path.display()),
            )
            .into());
        }
        Ok(Self {
            path: path.to_path_buf(),
            len,
            modified,
            digest: hasher.finalize().into(),
        })
    }

    /// Capture file metadata while using a digest already computed by a
    /// streaming parser. The digest and metadata are verified again when the
    /// package is written.
    pub fn snapshot_with_digest(
        path: impl AsRef<Path>,
        digest: [u8; 20],
    ) -> Result<Self, OpcError> {
        let path = path.as_ref();
        let metadata = std::fs::metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("file-backed part source is not a file: {}", path.display()),
            )
            .into());
        }
        Ok(Self {
            path: path.to_path_buf(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            digest,
        })
    }

    /// Captured file length in bytes.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Whether the captured file is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn into_parts(self) -> (PathBuf, u64, Option<SystemTime>, [u8; 20]) {
        (self.path, self.len, self.modified, self.digest)
    }

    pub(crate) fn read_verified(&self) -> Result<Vec<u8>, OpcError> {
        Ok(read_verified_file(
            &self.path,
            self.len,
            self.modified,
            self.digest,
        )?)
    }
}

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

    pub(crate) fn reopen(&self, path: &Path) -> Result<(), OpcError> {
        *self.lock()? = Some(File::open(path)?);
        Ok(())
    }
}

enum PartData {
    Loaded(Vec<u8>),
    FileBacked {
        path: PathBuf,
        len: u64,
        modified: Option<SystemTime>,
        digest: [u8; 20],
        cache: OnceLock<Result<Vec<u8>, String>>,
    },
    Lazy {
        source: Arc<LazyArchive>,
        entry_index: usize,
        len: u64,
        cache: OnceLock<Result<Vec<u8>, String>>,
    },
}

impl Clone for PartData {
    fn clone(&self) -> Self {
        fn clone_cache(
            source: &OnceLock<Result<Vec<u8>, String>>,
        ) -> OnceLock<Result<Vec<u8>, String>> {
            let target = OnceLock::new();
            if let Some(value) = source.get() {
                let _ = target.set(value.clone());
            }
            target
        }

        match self {
            Self::Loaded(data) => Self::Loaded(data.clone()),
            Self::FileBacked {
                path,
                len,
                modified,
                digest,
                cache,
            } => Self::FileBacked {
                path: path.clone(),
                len: *len,
                modified: *modified,
                digest: *digest,
                cache: clone_cache(cache),
            },
            Self::Lazy {
                source,
                entry_index,
                len,
                cache,
            } => Self::Lazy {
                source: Arc::clone(source),
                entry_index: *entry_index,
                len: *len,
                cache: clone_cache(cache),
            },
        }
    }
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

impl Clone for Part {
    fn clone(&self) -> Self {
        Self {
            uri: self.uri.clone(),
            data: self.data.clone(),
            source_index: self.source_index,
            modified: self.modified,
            dir: self.dir,
            relationships: self.relationships.clone(),
            compression: self.compression,
            last_modified: self.last_modified,
        }
    }
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
        len: u64,
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
                    len,
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

    pub(crate) fn new_file_backed(
        uri: PartUri,
        path: PathBuf,
        len: u64,
        modified: Option<SystemTime>,
        digest: [u8; 20],
    ) -> Self {
        Self {
            uri,
            data: PartData::FileBacked {
                path,
                len,
                modified,
                digest,
                cache: OnceLock::new(),
            },
            source_index: None,
            modified: false,
            dir: false,
            relationships: None,
            compression: CompressionMethod::Deflated,
            last_modified: None,
        }
    }

    pub(crate) fn set_relationships(&mut self, rels: Relationships) {
        self.relationships = Some(rels);
    }

    pub(crate) fn replace_bytes(&mut self, data: Vec<u8>) {
        self.data = PartData::Loaded(data);
        self.modified = true;
    }

    pub(crate) fn replace_file_backed(&mut self, source: FilePartSource) {
        let (path, len, modified, digest) = source.into_parts();
        self.data = PartData::FileBacked {
            path,
            len,
            modified,
            digest,
            cache: OnceLock::new(),
        };
        self.source_index = None;
        self.modified = true;
        self.dir = false;
        self.last_modified = None;
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

    pub(crate) fn is_file_backed(&self) -> bool {
        matches!(self.data, PartData::FileBacked { .. })
    }

    pub(crate) fn resident_len(&self) -> u64 {
        match &self.data {
            PartData::Loaded(data) => data.len() as u64,
            PartData::FileBacked { cache, .. } | PartData::Lazy { cache, .. } => cache
                .get()
                .and_then(|result| result.as_ref().ok())
                .map_or(0, |data| data.len() as u64),
        }
    }

    pub(crate) fn evictable_len(&self) -> u64 {
        if self.modified {
            return 0;
        }
        match &self.data {
            PartData::Lazy { cache, .. } => cache
                .get()
                .and_then(|result| result.as_ref().ok())
                .map_or(0, |data| data.len() as u64),
            PartData::Loaded(_) | PartData::FileBacked { .. } => 0,
        }
    }

    pub(crate) fn is_evictable(&self) -> bool {
        !self.modified
            && matches!(&self.data, PartData::Lazy { cache, .. } if cache.get().is_some())
    }

    pub(crate) fn evict_clean_cache(&mut self) -> Option<u64> {
        if self.modified {
            return None;
        }
        match &mut self.data {
            PartData::Lazy { cache, .. } => cache
                .take()
                .map(|result| result.map_or(0, |data| data.len() as u64)),
            PartData::Loaded(_) | PartData::FileBacked { .. } => None,
        }
    }

    pub(crate) fn content_len(&self) -> Result<u64, OpcError> {
        match &self.data {
            PartData::Loaded(data) => Ok(data.len() as u64),
            PartData::FileBacked { len, .. } => Ok(*len),
            PartData::Lazy { len, .. } => Ok(*len),
        }
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
            PartData::FileBacked {
                path,
                len,
                modified,
                digest,
                cache,
            } => match cache.get_or_init(|| {
                read_verified_file(path, *len, *modified, *digest)
                    .map_err(|error| error.to_string())
            }) {
                Ok(data) => Ok(data),
                Err(detail) => Err(OpcError::ZipRead {
                    detail: format!("entry {:?}: {detail}", self.name()),
                }),
            },
            PartData::Lazy {
                source,
                entry_index,
                len,
                cache,
                ..
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
                    .take(len.saturating_add(1))
                    .read_to_end(&mut data)
                    .map_err(|error| error.to_string())?;
                if data.len() as u64 != *len {
                    return Err(format!(
                        "source ZIP changed: expected {len} uncompressed bytes, got {}",
                        data.len()
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

    pub(crate) fn write_content(&self, writer: &mut impl Write) -> Result<(), OpcError> {
        match &self.data {
            PartData::FileBacked {
                path,
                len,
                modified,
                digest,
                ..
            } => stream_verified_file(path, *len, *modified, *digest, writer),
            _ => {
                writer.write_all(self.bytes()?)?;
                Ok(())
            }
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
            PartData::FileBacked { cache, .. } => cache.get().is_some(),
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
                    PartData::FileBacked { cache, .. } => cache
                        .get()
                        .and_then(|result| result.as_ref().ok())
                        .map(Vec::len),
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

fn validate_file_metadata(
    path: &std::path::Path,
    len: u64,
    modified: Option<SystemTime>,
) -> std::io::Result<()> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() != len || metadata.modified().ok() != modified {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file-backed part source changed: {}", path.display()),
        ));
    }
    Ok(())
}

fn stream_verified_file(
    path: &std::path::Path,
    len: u64,
    modified: Option<SystemTime>,
    expected_digest: [u8; 20],
    writer: &mut impl Write,
) -> Result<(), OpcError> {
    validate_file_metadata(path, len, modified)?;
    let mut file = File::open(path)?;
    let mut hasher = Sha1::new();
    let mut written = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        written = written.saturating_add(read as u64);
        if written > len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("file-backed part source changed: {}", path.display()),
            )
            .into());
        }
        hasher.update(&buffer[..read]);
        writer.write_all(&buffer[..read])?;
    }
    validate_file_metadata(path, len, modified)?;
    let actual: [u8; 20] = hasher.finalize().into();
    if written != len || actual != expected_digest {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("file-backed part source changed: {}", path.display()),
        )
        .into());
    }
    Ok(())
}

fn read_verified_file(
    path: &std::path::Path,
    len: u64,
    modified: Option<SystemTime>,
    digest: [u8; 20],
) -> std::io::Result<Vec<u8>> {
    let capacity = usize::try_from(len).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "file-backed part is too large for this platform",
        )
    })?;
    let mut bytes = Vec::with_capacity(capacity);
    stream_verified_file(path, len, modified, digest, &mut bytes).map_err(|error| match error {
        OpcError::Io(error) => error,
        other => std::io::Error::other(other.to_string()),
    })?;
    Ok(bytes)
}

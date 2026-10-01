use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use docxtpl_template::PreparedXmlTemplate;
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"DXTPC001";
const SCHEMA: u32 = 1;
const NAMESPACE: &str = "docxtpl-rs-prepared-v1";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Explicit capacity and lifetime policy for persistent prepared templates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PreparedCachePolicy {
    pub max_entries: usize,
    pub max_bytes: u64,
    pub max_entry_bytes: u64,
    pub ttl: Duration,
}

impl Default for PreparedCachePolicy {
    fn default() -> Self {
        Self {
            max_entries: 512,
            max_bytes: 256 * 1024 * 1024,
            max_entry_bytes: 32 * 1024 * 1024,
            ttl: Duration::from_secs(30 * 24 * 60 * 60),
        }
    }
}

/// Observable cumulative outcomes for one cache handle and its clones.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreparedCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub writes: u64,
    pub corruptions: u64,
    pub expirations: u64,
    pub evictions: u64,
    pub io_errors: u64,
}

#[derive(Debug, Default)]
struct CacheCounters {
    hits: AtomicU64,
    misses: AtomicU64,
    writes: AtomicU64,
    corruptions: AtomicU64,
    expirations: AtomicU64,
    evictions: AtomicU64,
    io_errors: AtomicU64,
}

/// Opt-in disk cache for context-independent XML preprocessing.
///
/// Files are isolated below a cache-owned namespace directory. Clones share
/// counters and an in-process mutation lock; independent processes coordinate
/// through atomic same-directory renames and validated immutable entries.
#[derive(Debug, Clone)]
pub struct PreparedTemplateCache {
    inner: Arc<CacheInner>,
}

#[derive(Debug)]
struct CacheInner {
    directory: PathBuf,
    policy: PreparedCachePolicy,
    counters: CacheCounters,
    mutation: Mutex<()>,
}

impl PreparedTemplateCache {
    pub fn new(root: impl AsRef<Path>, policy: PreparedCachePolicy) -> std::io::Result<Self> {
        let directory = root.as_ref().join(NAMESPACE);
        fs::create_dir_all(&directory)?;
        let cache = Self {
            inner: Arc::new(CacheInner {
                directory,
                policy,
                counters: CacheCounters::default(),
                mutation: Mutex::new(()),
            }),
        };
        cache.enforce_policy();
        Ok(cache)
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.inner.directory
    }

    #[must_use]
    pub fn policy(&self) -> PreparedCachePolicy {
        self.inner.policy
    }

    #[must_use]
    pub fn stats(&self) -> PreparedCacheStats {
        let load = |value: &AtomicU64| value.load(Ordering::Relaxed);
        PreparedCacheStats {
            hits: load(&self.inner.counters.hits),
            misses: load(&self.inner.counters.misses),
            writes: load(&self.inner.counters.writes),
            corruptions: load(&self.inner.counters.corruptions),
            expirations: load(&self.inner.counters.expirations),
            evictions: load(&self.inner.counters.evictions),
            io_errors: load(&self.inner.counters.io_errors),
        }
    }

    /// Remove cache entries owned by this namespace. Other files and
    /// subdirectories are untouched.
    pub fn clear(&self) -> std::io::Result<usize> {
        let _guard = self
            .inner
            .mutation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut removed = 0;
        for entry in fs::read_dir(&self.inner.directory)? {
            let path = entry?.path();
            if is_cache_entry(&path) {
                fs::remove_file(path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub(crate) fn load(
        &self,
        template_digest: &[u8; 32],
        options_fingerprint: &[u8; 32],
        kind: u8,
        part_name: &str,
        source: &str,
    ) -> Option<PreparedXmlTemplate> {
        let _guard = self
            .inner
            .mutation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let source_digest = digest(source.as_bytes());
        let path = self.entry_path(
            template_digest,
            options_fingerprint,
            kind,
            part_name,
            &source_digest,
        );
        let metadata = match fs::metadata(&path) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.bump(&self.inner.counters.misses);
                return None;
            }
            Err(_) => {
                self.bump(&self.inner.counters.io_errors);
                self.bump(&self.inner.counters.misses);
                return None;
            }
        };
        if metadata.len() > self.inner.policy.max_entry_bytes {
            let _ = fs::remove_file(&path);
            self.bump(&self.inner.counters.corruptions);
            self.bump(&self.inner.counters.misses);
            return None;
        }
        if self.expired(&metadata) {
            let _ = fs::remove_file(&path);
            self.bump(&self.inner.counters.expirations);
            self.bump(&self.inner.counters.misses);
            return None;
        }
        let Ok(capacity) = usize::try_from(metadata.len()) else {
            let _ = fs::remove_file(&path);
            self.bump(&self.inner.counters.corruptions);
            self.bump(&self.inner.counters.misses);
            return None;
        };
        let mut bytes = Vec::with_capacity(capacity);
        let loaded = File::open(&path).and_then(|mut file| file.read_to_end(&mut bytes));
        if loaded.is_err() {
            self.bump(&self.inner.counters.io_errors);
            self.bump(&self.inner.counters.misses);
            return None;
        }
        match decode_entry(
            &bytes,
            template_digest,
            options_fingerprint,
            kind,
            part_name,
            &source_digest,
        ) {
            Some(template) => {
                self.bump(&self.inner.counters.hits);
                Some(template)
            }
            None => {
                let _ = fs::remove_file(&path);
                self.bump(&self.inner.counters.corruptions);
                self.bump(&self.inner.counters.misses);
                None
            }
        }
    }

    pub(crate) fn store(
        &self,
        template_digest: &[u8; 32],
        options_fingerprint: &[u8; 32],
        kind: u8,
        part_name: &str,
        source: &str,
        template: &PreparedXmlTemplate,
    ) {
        let _guard = self
            .inner
            .mutation
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let source_digest = digest(source.as_bytes());
        let bytes = encode_entry(
            template_digest,
            options_fingerprint,
            kind,
            part_name,
            &source_digest,
            template,
        );
        if bytes.len() as u64 > self.inner.policy.max_entry_bytes {
            self.bump(&self.inner.counters.io_errors);
            return;
        }
        let destination = self.entry_path(
            template_digest,
            options_fingerprint,
            kind,
            part_name,
            &source_digest,
        );
        if destination.exists() {
            return;
        }
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary = self
            .inner
            .directory
            .join(format!(".tmp-{}-{sequence}.dptc", std::process::id()));
        let mut installed = false;
        let result = (|| -> std::io::Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            match fs::rename(&temporary, &destination) {
                Ok(()) => {
                    installed = true;
                    Ok(())
                }
                Err(_error) if destination.exists() => {
                    let _ = fs::remove_file(&temporary);
                    Ok(())
                }
                Err(error) => Err(error),
            }
        })();
        if result.is_ok() {
            if installed {
                self.bump(&self.inner.counters.writes);
            }
            self.enforce_policy();
        } else {
            let _ = fs::remove_file(&temporary);
            self.bump(&self.inner.counters.io_errors);
        }
    }

    fn entry_path(
        &self,
        template_digest: &[u8; 32],
        options_fingerprint: &[u8; 32],
        kind: u8,
        part_name: &str,
        source_digest: &[u8; 32],
    ) -> PathBuf {
        let mut hasher = Sha256::new();
        hasher.update(template_digest);
        hasher.update(options_fingerprint);
        hasher.update([kind]);
        hasher.update(part_name.as_bytes());
        hasher.update(source_digest);
        self.inner
            .directory
            .join(format!("{}.dptc", hex(&hasher.finalize())))
    }

    fn expired(&self, metadata: &fs::Metadata) -> bool {
        metadata
            .modified()
            .ok()
            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
            .is_some_and(|age| age > self.inner.policy.ttl)
    }

    fn enforce_policy(&self) {
        let Ok(entries) = fs::read_dir(&self.inner.directory) else {
            self.bump(&self.inner.counters.io_errors);
            return;
        };
        let mut files = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if !is_cache_entry(&path) {
                continue;
            }
            if let Ok(metadata) = entry.metadata() {
                if self.expired(&metadata) {
                    if fs::remove_file(&path).is_ok() {
                        self.bump(&self.inner.counters.expirations);
                    }
                } else {
                    files.push((
                        metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                        metadata.len(),
                        path,
                    ));
                }
            }
        }
        files.sort_by_key(|(modified, _, path)| (*modified, path.clone()));
        let mut total: u64 = files.iter().map(|(_, size, _)| *size).sum();
        let mut count = files.len();
        for (_, size, path) in files {
            if count <= self.inner.policy.max_entries && total <= self.inner.policy.max_bytes {
                break;
            }
            if fs::remove_file(path).is_ok() {
                count -= 1;
                total = total.saturating_sub(size);
                self.bump(&self.inner.counters.evictions);
            }
        }
    }

    fn bump(&self, counter: &AtomicU64) {
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

fn encode_entry(
    template_digest: &[u8; 32],
    options_fingerprint: &[u8; 32],
    kind: u8,
    part_name: &str,
    source_digest: &[u8; 32],
    template: &PreparedXmlTemplate,
) -> Vec<u8> {
    let (prepared, shape_id) = template.cache_parts();
    let version = env!("CARGO_PKG_VERSION").as_bytes();
    let mut bytes = Vec::new();
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&SCHEMA.to_le_bytes());
    bytes.extend_from_slice(&(version.len() as u16).to_le_bytes());
    bytes.extend_from_slice(version);
    bytes.extend_from_slice(template_digest);
    bytes.extend_from_slice(options_fingerprint);
    bytes.extend_from_slice(source_digest);
    bytes.push(kind);
    bytes.extend_from_slice(&(part_name.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&(prepared.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&shape_id.to_le_bytes());
    bytes.extend_from_slice(part_name.as_bytes());
    bytes.extend_from_slice(prepared.as_bytes());
    let checksum = digest(&bytes);
    bytes.extend_from_slice(&checksum);
    bytes
}

fn decode_entry(
    bytes: &[u8],
    template_digest: &[u8; 32],
    options_fingerprint: &[u8; 32],
    kind: u8,
    part_name: &str,
    source_digest: &[u8; 32],
) -> Option<PreparedXmlTemplate> {
    if bytes.len() < 8 + 4 + 2 + 32 * 4 + 1 + 4 + 8 + 8 || &bytes[..8] != MAGIC {
        return None;
    }
    let payload_len = bytes.len().checked_sub(32)?;
    if digest(&bytes[..payload_len]).as_slice() != &bytes[payload_len..] {
        return None;
    }
    let mut cursor = 8;
    if take_u32(bytes, &mut cursor)? != SCHEMA {
        return None;
    }
    let version_len = take_u16(bytes, &mut cursor)? as usize;
    if take(bytes, &mut cursor, version_len)? != env!("CARGO_PKG_VERSION").as_bytes()
        || take(bytes, &mut cursor, 32)? != template_digest
        || take(bytes, &mut cursor, 32)? != options_fingerprint
        || take(bytes, &mut cursor, 32)? != source_digest
        || *take(bytes, &mut cursor, 1)?.first()? != kind
    {
        return None;
    }
    let name_len = take_u32(bytes, &mut cursor)? as usize;
    let prepared_len = usize::try_from(take_u64(bytes, &mut cursor)?).ok()?;
    let shape_id = i64::from_le_bytes(take(bytes, &mut cursor, 8)?.try_into().ok()?);
    if take(bytes, &mut cursor, name_len)? != part_name.as_bytes() {
        return None;
    }
    let prepared = std::str::from_utf8(take(bytes, &mut cursor, prepared_len)?)
        .ok()?
        .to_string();
    (cursor == payload_len).then(|| PreparedXmlTemplate::from_cache_parts(prepared, shape_id))
}

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn digest_path(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn take<'a>(bytes: &'a [u8], cursor: &mut usize, count: usize) -> Option<&'a [u8]> {
    let end = cursor.checked_add(count)?;
    let value = bytes.get(*cursor..end)?;
    *cursor = end;
    Some(value)
}

fn take_u16(bytes: &[u8], cursor: &mut usize) -> Option<u16> {
    Some(u16::from_le_bytes(take(bytes, cursor, 2)?.try_into().ok()?))
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> Option<u32> {
    Some(u32::from_le_bytes(take(bytes, cursor, 4)?.try_into().ok()?))
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    Some(u64::from_le_bytes(take(bytes, cursor, 8)?.try_into().ok()?))
}

fn is_cache_entry(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension == "dptc")
        && path
            .file_name()
            .is_some_and(|name| !name.to_string_lossy().starts_with(".tmp-"))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

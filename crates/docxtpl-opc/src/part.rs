//! 包内单个 part。

use std::fmt;

use zip::CompressionMethod;
use zip::DateTime;

use crate::rels::Relationships;
use crate::uri::PartUri;

/// 单个 part（含目录条目：`is_dir() == true` 且 `bytes()` 为空）。
///
/// 通过 [`crate::Package::parts`] / [`crate::Package::part`] 获取；
/// 修改内容请使用 [`crate::Package::set_part_bytes`]。
///
/// # 示例
///
/// ```no_run
/// use docxtpl_opc::{Package, PackageLimits};
///
/// let pkg = Package::open("template.docx", &PackageLimits::default())?;
/// let doc = pkg.part("word/document.xml").expect("主文档存在");
/// assert_eq!(doc.name(), "word/document.xml");
/// assert!(!doc.is_modified());
/// # Ok::<(), docxtpl_opc::OpcError>(())
/// ```
pub struct Part {
    uri: PartUri,
    data: Vec<u8>,
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
            data,
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
        self.data = data;
        self.modified = true;
    }

    pub(crate) fn compression_method(&self) -> CompressionMethod {
        self.compression
    }

    pub(crate) fn last_modified(&self) -> Option<DateTime> {
        self.last_modified
    }

    /// part URI。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let doc = pkg.part("word/document.xml").expect("主文档存在");
    /// assert_eq!(doc.uri().as_str(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn uri(&self) -> &PartUri {
        &self.uri
    }

    /// 条目名（即 `uri().as_str()`）。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert_eq!(pkg.part("word/document.xml").expect("主文档存在").name(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn name(&self) -> &str {
        self.uri.as_str()
    }

    /// 当前内容字节（被修改后为新字节）。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let doc = pkg.part("word/document.xml").expect("主文档存在");
    /// let content: &[u8] = doc.bytes();
    /// assert!(!content.is_empty());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }

    /// 该 part 的关系集合；没有对应 `_rels/<name>.rels` 时为 `None`。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// // 依赖 word/_rels/document.xml.rels 是否存在
    /// let rels = pkg.part("word/document.xml").expect("主文档存在").relationships();
    /// assert_eq!(rels.is_some(), pkg.contains("word/_rels/document.xml.rels"));
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn relationships(&self) -> Option<&Relationships> {
        self.relationships.as_ref()
    }

    /// 是否是目录条目（条目名以 `/` 结尾）。
    ///
    /// # 示例
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

    /// 内容是否被 [`crate::Package::set_part_bytes`] 修改过。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// # let mut pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert!(!pkg.part("word/document.xml").expect("主文档存在").is_modified());
    /// pkg.set_part_bytes("word/document.xml", b"<w:document/>".to_vec())?;
    /// assert!(pkg.part("word/document.xml").expect("主文档存在").is_modified());
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
            .field("len", &self.data.len())
            .field("modified", &self.modified)
            .field("dir", &self.dir)
            .field(
                "relationships",
                &self.relationships.as_ref().map(Relationships::len),
            )
            .finish()
    }
}

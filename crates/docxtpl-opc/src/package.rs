//! OPC 包：打开（限额）、关系/Content Types 索引、校验与写回。

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::path::Path;

use zip::read::ZipArchive;
use zip::write::{SimpleFileOptions, ZipWriter};
use zip::CompressionMethod;

use crate::content_types::ContentTypes;
use crate::error::OpcError;
use crate::limits::PackageLimits;
use crate::part::Part;
use crate::rels::{Relationships, TargetMode};
use crate::uri::{
    collapse_segments, is_rels_path, owner_of_rels_path, percent_decode, rels_path_for,
    resolve_relative_to, PartUri,
};

/// officeDocument 关系类型（主文档）。
const OFFICE_DOCUMENT_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument";

/// OPC 包（OOXML 的 ZIP 容器 + Content Types + relationships）。
///
/// 典型流程见 crate 级文档。
pub struct Package {
    limits: PackageLimits,
    parts: Vec<Part>,
    /// 原始条目名 → parts 下标。
    index: HashMap<String, usize>,
    content_types: ContentTypes,
    root_rels: Relationships,
}

impl Package {
    /// 从文件打开 OPC 包。
    ///
    /// # 失败情况
    ///
    /// [`OpcError::Io`]：文件无法打开；其余同 [`Package::from_reader`]。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// use docxtpl_opc::{Package, PackageLimits};
    ///
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert!(pkg.contains("[Content_Types].xml"));
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn open(path: impl AsRef<Path>, limits: &PackageLimits) -> Result<Self, OpcError> {
        let file = std::fs::File::open(path)?;
        Self::from_reader(file, limits)
    }

    /// 从任意可寻址读取流解析 OPC 包。
    ///
    /// 全部条目内容会读入内存（docx 规模小，P1 不做流式）；读取过程受
    /// `limits` 约束：条目数、单条目与总解压大小在读字节前先查 ZIP 元数据，
    /// 压缩比在读完后核查，用于拦截 zip 炸弹。
    ///
    /// # 失败情况
    ///
    /// - [`OpcError::LimitExceeded`]：超出限额（kind 为 `"entries"`、
    ///   `"entry_uncompressed"`、`"total_uncompressed"` 或 `"compression_ratio"`）
    /// - [`OpcError::InvalidUri`]：条目名非法（绝对路径、`..`、反斜杠等）
    /// - [`OpcError::DuplicateEntry`]：条目名按 exact/case/percent 任一口径冲突
    /// - [`OpcError::ZipRead`]：ZIP 数据损坏或加密
    /// - [`OpcError::Malformed`]：缺少 [Content_Types].xml 或 _rels/.rels
    /// - [`OpcError::InvalidContentTypes`] / [`OpcError::InvalidRelationships`]：XML 解析失败
    ///
    /// # 示例
    ///
    /// ```no_run
    /// use std::io::Cursor;
    ///
    /// use docxtpl_opc::{Package, PackageLimits};
    ///
    /// # let bytes: Vec<u8> = Vec::new(); // 实际为 OPC 包字节
    /// let pkg = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    /// assert!(pkg.part_count() > 0);
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn from_reader<R: Read + Seek>(
        reader: R,
        limits: &PackageLimits,
    ) -> Result<Self, OpcError> {
        let mut reader = reader;

        // 先扫原始中央目录：zip crate 读取时用 IndexMap 折叠重名条目，
        // 精确重复必须自己解析中央目录才能看到。扫描失败则退回 zip crate 的
        // （重名折叠后的）视角，此时仅能检测 case/percent 口径。
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

        // 重复检测三口径（扫描成功时含精确重名，在读任何字节前完成）。
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

            // 读字节前先检查元数据，拦截 zip 炸弹。
            let declared = entry.size();
            if declared > limits.max_entry_uncompressed {
                return Err(OpcError::LimitExceeded {
                    kind: "entry_uncompressed",
                    value: declared,
                    max: limits.max_entry_uncompressed,
                });
            }

            let is_dir = name.ends_with('/');
            let data = if is_dir {
                // 目录条目：保留为 is_dir part，内容为空。
                Vec::new()
            } else {
                let remaining = limits
                    .max_total_uncompressed
                    .saturating_sub(total_uncompressed);
                let mut buf = Vec::new();
                entry
                    .by_ref()
                    .take(remaining.saturating_add(1))
                    .read_to_end(&mut buf)
                    .map_err(|err| OpcError::ZipRead {
                        detail: format!("条目 {name:?}: {err}"),
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
                // Stored 条目 compressed == uncompressed，天然满足任意压缩比限额；
                // compressed == 0（零长度条目）同样不检查。
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
            };

            let compression = entry.compression();
            let last_modified = entry.last_modified();
            index.insert(name, parts.len());
            parts.push(Part::new(uri, data, is_dir, compression, last_modified));
        }

        let mut package = Package {
            limits: limits.clone(),
            parts,
            index,
            content_types: ContentTypes::default(),
            root_rels: Relationships::default(),
        };

        // [Content_Types].xml 必须存在。
        let ct_xml = package
            .part("[Content_Types].xml")
            .map(|part| String::from_utf8_lossy(part.bytes()).into_owned())
            .ok_or_else(|| OpcError::Malformed {
                reason: "缺少 [Content_Types].xml".to_string(),
            })?;
        package.content_types = ContentTypes::parse(&ct_xml)?;

        // _rels/.rels 必须存在。
        let root_xml = package
            .part("_rels/.rels")
            .map(|part| String::from_utf8_lossy(part.bytes()).into_owned())
            .ok_or_else(|| OpcError::Malformed {
                reason: "缺少 _rels/.rels".to_string(),
            })?;
        package.root_rels = Relationships::parse_in(&root_xml, "_rels/.rels")?;

        // 每对 <dir>/_rels/<name>.rels 归属给同目录的 <dir>/<name> part。
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
            let xml = String::from_utf8_lossy(package.parts[rels_index].bytes()).into_owned();
            attached.push(Some(Relationships::parse_in(&xml, &rels_path)?));
        }
        for (part, rels) in package.parts.iter_mut().zip(attached) {
            if let Some(rels) = rels {
                part.set_relationships(rels);
            }
        }

        Ok(package)
    }

    /// 打开时使用的限额。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// assert_eq!(pkg.limits().max_entries, 1_000);
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn limits(&self) -> &PackageLimits {
        &self.limits
    }

    /// 按原始 ZIP 顺序迭代全部 part（含目录条目）。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// for part in pkg.parts() {
    ///     println!("{} ({} 字节)", part.name(), part.bytes().len());
    /// }
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn parts(&self) -> impl Iterator<Item = &Part> {
        self.parts.iter()
    }

    /// part 总数（含目录条目）。
    ///
    /// # 示例
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

    /// 按条目名精确查找 part（名称须与 ZIP 条目名一致）。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let doc = pkg.part("word/document.xml").expect("主文档存在");
    /// assert_eq!(doc.name(), "word/document.xml");
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn part(&self, name: &str) -> Option<&Part> {
        self.index.get(name).map(|&i| &self.parts[i])
    }

    /// 是否存在指定条目名的 part。
    ///
    /// # 示例
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

    /// 修改 part 内容并标记 modified。
    ///
    /// 若目标是 [Content_Types].xml 或 `.rels` 文件，会同步重新解析对应的
    /// content types / 关系视图；解析失败则整个调用失败，字节保持不变。
    ///
    /// # 失败情况
    ///
    /// - [`OpcError::PartNotFound`]：条目不存在
    /// - [`OpcError::InvalidContentTypes`] / [`OpcError::InvalidRelationships`]：新字节不是合法的对应 XML
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let mut pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// pkg.set_part_bytes("word/document.xml", b"<w:document/>".to_vec())?;
    /// assert_eq!(pkg.part("word/document.xml").expect("主文档存在").bytes(), b"<w:document/>");
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

    /// 按 python-docx `_ContentTypesItem.from_parts`（0.20.2）从当前包部件
    /// 重建 content types 视图。
    ///
    /// 枚举除 [Content_Types].xml、`.rels` 与目录条目外的全部 part，按
    /// 各自当前 content type 重排 Default/Override 归属（真实 Word 模板
    /// 里 rels/customXml 的 Override 在此消失，rels/xml Default 恒在）。
    /// 本方法只改内存视图；调用方随后取 [`Package::content_types`] 的
    /// `to_xml()` 并 [`Package::set_part_bytes`] 落盘。
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

    /// 把包内全部 `.rels`（根 rels 与每个挂接 part 的 rels）重写为模型
    /// 规范序列化（lxml 单引号声明），对齐 python-docx `PackageWriter`
    /// 保存时**始终重写** rels 流的行为（真实 Word 模板的双引号声明、
    /// 尾部换行会被归一）。字节与当前条目一致时不写回、不标修改。
    ///
    /// # 失败情况
    ///
    /// 规范 XML 必须能通过 [`Relationships::parse_in`]（模型本身的输出
    /// 恒可解析）；写回异常仅可能来自 [`Package::set_part_bytes`]。
    pub fn normalize_relationships(&mut self) -> Result<(), OpcError> {
        let root_xml = self.root_rels.to_xml();
        if self
            .part("_rels/.rels")
            .is_none_or(|part| part.bytes() != root_xml.as_bytes())
        {
            self.set_part_bytes("_rels/.rels", root_xml.into_bytes())?;
        }
        // 先收集 (rels 路径, 规范字节) 再写回，避免遍历与 &mut 冲突。
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
            if self
                .part(&rels_path)
                .is_none_or(|part| part.bytes() != bytes.as_slice())
            {
                self.set_part_bytes(&rels_path, bytes)?;
            }
        }
        Ok(())
    }

    /// 追加一个新 part（如渲染注入的 `word/media/imageN.*`）。
    ///
    /// 新 part 以 Deflate 压缩、默认 ZIP 时间戳写出（对齐 python-docx
    /// `PackageWriter` 新增图片 part 的写出方式）；条目尾插在 parts 末尾。
    ///
    /// # 失败情况
    ///
    /// - [`OpcError::InvalidUri`]：条目名非法
    /// - [`OpcError::DuplicateEntry`]：条目名已存在（精确匹配；新增路径由
    ///   调用方按 image 编号分配，构造场景下不会出现 case/percent 变体）
    ///
    /// # 示例
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

    /// [Content_Types].xml 的解析视图。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// let uri = pkg.part("word/document.xml").expect("主文档存在").uri().clone();
    /// assert!(pkg.content_types().content_type_of(&uri).is_some());
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn content_types(&self) -> &ContentTypes {
        &self.content_types
    }

    /// 根关系（`_rels/.rels`）。
    ///
    /// # 示例
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

    /// 指定 part 的关系集合（等价于 `part(name)?.relationships()`）。
    ///
    /// # 示例
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

    /// 主文档 part URI：根关系中 rel_type 为 officeDocument 的内部关系目标
    /// （相对包根解析）。
    ///
    /// # 失败情况
    ///
    /// [`OpcError::Malformed`]：没有 officeDocument 内部关系，或其目标无法
    /// 解析为包内 URI。目标 part 是否存在由 [`Package::validate`] 检查。
    ///
    /// # 示例
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
                        "officeDocument 关系目标 {:?} 无法解析为包内 URI",
                        rel.target
                    ),
                });
            }
        }
        Err(OpcError::Malformed {
            reason: "根关系缺少 officeDocument 关系".to_string(),
        })
    }

    /// 校验包完整性：
    ///
    /// 1. [Content_Types].xml、`_rels/.rels` 与主文档目标必须存在；
    /// 2. 每条内部关系 resolve 后必须命中存在的 part（悬空即错）；
    /// 3. 每个 rels 文件内 Id 唯一；
    /// 4. 除 [Content_Types].xml、`_rels/*` 与目录条目外，每个 part 都必须有
    ///    content type。
    ///
    /// # 失败情况
    ///
    /// - [`OpcError::MissingPart`]：必需 part 缺失
    /// - [`OpcError::Malformed`]：根关系缺少 officeDocument
    /// - [`OpcError::InvalidRelationships`]：内部关系悬空或 Id 重复
    /// - [`OpcError::InvalidContentTypes`]：普通 part 缺少 content type
    ///
    /// # 示例
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
                // 与 Relationships::resolve 一致：目标相对 part 所在目录解析
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
                    reason: format!("part {:?} 没有对应的 content type", part.name()),
                });
            }
        }
        Ok(())
    }

    /// 保存到文件。
    ///
    /// tempfile 只是 dev 依赖，库内无法使用；P1 策略为：先在内存中完成整个
    /// 输出（受 `max_output_size` 限制），成功后一次性落盘，限额失败时不会
    /// 留下半截文件。写出细节见 [`Package::write_to`]。
    ///
    /// # 失败情况
    ///
    /// [`OpcError::LimitExceeded`]（kind `"output"`）、[`OpcError::ZipWrite`] 或
    /// [`OpcError::Io`]。
    ///
    /// # 示例
    ///
    /// ```no_run
    /// # use docxtpl_opc::{Package, PackageLimits};
    /// let pkg = Package::open("template.docx", &PackageLimits::default())?;
    /// pkg.save("out.docx")?;
    /// # Ok::<(), docxtpl_opc::OpcError>(())
    /// ```
    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), OpcError> {
        let mut cursor = Cursor::new(Vec::new());
        self.write_to(&mut cursor)?;
        std::fs::write(path, cursor.into_inner())?;
        Ok(())
    }

    /// 写出到任意可寻址写入流。
    ///
    /// 按 [`Package::parts`] 原顺序逐条写出：未修改 part 写原始字节，修改过的
    /// 写新字节；条目名原样；保留原压缩方式（Stored→Stored，其余→Deflated
    /// 默认级别）；时间戳能从原条目取到就复用，取不到用默认；目录条目写为
    /// 零长度 Stored。
    ///
    /// # 失败情况
    ///
    /// - [`OpcError::LimitExceeded`]：总输出超过 `max_output_size`
    ///   （kind `"output"`，计数写包装器写满即报错）
    /// - [`OpcError::ZipWrite`]：底层写出失败
    ///
    /// # 示例
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
        let max = self.limits.max_output_size;
        let mut limited = CountingWriter::new(&mut writer, max);
        let result = write_zip(&self.parts, &mut limited);
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
                    Err(OpcError::ZipWrite {
                        detail: err.to_string(),
                    })
                }
            }
        }
    }

    /// 校验一组关系的内部目标都能命中包内存在的 part。
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
                        "{source}: 关系 {} 的目标 {:?} 未命中包内 part",
                        rel.id, rel.target
                    ),
                });
            }
        }
        Ok(())
    }

    /// 校验一组关系内的 Id 唯一。
    fn check_rels_ids(&self, rels: &Relationships, source: &str) -> Result<(), OpcError> {
        let mut seen: HashSet<&str> = HashSet::new();
        for rel in rels.iter() {
            if !seen.insert(rel.id.as_str()) {
                return Err(OpcError::InvalidRelationships {
                    reason: format!("{source}: 重复关系 Id {}", rel.id),
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

/// 计数写包装器：按流的逻辑位置统计输出字节数（seek 同步位置，重复回写
/// 不重复计数），一旦会超过 `max` 就报错（写满即停，不留半截文件）。
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
                "输出超出 max_output_size 限额",
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

/// 按 parts 原顺序写出全部条目（见 [`Package::write_to`] 的行为说明）。
fn write_zip<W: Write + Seek>(
    parts: &[Part],
    out: &mut CountingWriter<W>,
) -> Result<(), zip::result::ZipError> {
    let mut zip = ZipWriter::new(out);
    for part in parts {
        let method = if part.is_dir() || part.compression_method() == CompressionMethod::Stored {
            CompressionMethod::Stored
        } else {
            CompressionMethod::Deflated
        };
        let mut options = SimpleFileOptions::default().compression_method(method);
        if let Some(time) = part.last_modified() {
            options = options.last_modified_time(time);
        }
        zip.start_file(part.name(), options)?;
        if !part.is_dir() {
            zip.write_all(part.bytes())?;
        }
    }
    zip.finish()?;
    Ok(())
}

/// 三口径重复条目检测器：精确（含空段折叠）、大小写折叠、百分号解码折叠
/// （先解码再折叠）。存储保留原始条目名，此处仅用于检测。
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

    /// 记录一个条目名；按任一口径冲突时返回 [`OpcError::DuplicateEntry`]。
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

/// 定位 ZIP 的 EOCD（含 zip64 sentinel 处理），返回
/// （中央目录声明的原始条目数，中央目录的实际起始位置）。
///
/// 实际位置按 `EOCD 位置 - cd_size` 推算，因此对带前置数据（如自解压头）的
/// ZIP 也成立。无法可靠解析时返回 Err，调用方退回 zip crate 的视角。
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
        return Err("文件比 EOCD 还短".to_string());
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

    // 从后往前找签名与注释长度自洽的 EOCD。
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
    let eocd = eocd.ok_or_else(|| "找不到 EOCD".to_string())?;

    let mut entries = u64::from(u16_at(&tail, eocd + 10));
    let cd_size = u64::from(u32_at(&tail, eocd + 12));

    // zip64 sentinel：EOCD64 locator 通常紧邻 EOCD 之前（20 字节）。
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

    // 实际中央目录位置 = EOCD 实际位置 - 中央目录大小（容纳前置数据）。
    let absolute_eocd = scan_start + eocd as u64;
    let cd_start = absolute_eocd
        .checked_sub(cd_size)
        .ok_or_else(|| "EOCD 声明的中央目录大小与位置矛盾".to_string())?;
    Ok((entries, cd_start))
}

/// 顺序读取中央目录的全部条目名（不去重）。名称用 lossy UTF-8，
/// 仅供重复检测；条目数已经过 max_entries 检查，内存可控。
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
            return Err("中央目录记录签名错误".to_string());
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

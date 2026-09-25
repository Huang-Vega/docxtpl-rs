//! P4 包级图片注册表（ADR-005）。
//!
//! [`ImageInjections`] 实现 docxtpl-template 的 [`ImageRegistry`]，把渲染期
//! 遇到的 [`InlineImage`] 落实为 OPC 包变更，语义逐一对齐 docxtpl 0.20.2 /
//! python-docx 1.2.0：
//!
//! - **全包 DFS** 收集既有 image 关系目标（`OpcPackage.iter_rels`，external
//!   跳过、visited 去重），作为编号与 sha1 去重的基线；
//! - **sha1 去重**：同字节图片复用既有 part（`ImageParts._get_by_sha1`）；
//! - **编号**：`word/media/imageN.ext`，N 从 1 起回填空洞，跨扩展名计数
//!   （`ImageParts._next_image_partname`，PackURI.idx 正则
//!   `^([a-zA-Z]+)([1-9][0-9]*)?`）；
//! - **rId**：rId1 起回填第一个空洞（含外部关系计数）；`reltype + 目标 +
//!   模式` 全等复用（`_Relationships.get_or_add(_ext)`）；
//! - **顺序**：图片 rId 先于其锚点 hyperlink 外链条目（上游
//!   `new_pic_anchor` 的分配次序）；
//! - **[Content_Types].xml**：新扩展名追加 Default（rels/xml 两 Default
//!   恒在，扩展名白名单内的图片走 Default 而非 Override）。
//!
//! 变更只暂存在本结构，渲染结束后由 [`ImageInjections::apply`] 一次性写入
//! [`Package`]：未涉及的 part（含无图片渲染时的 rels/CT 原字节）原样保留。

use std::collections::{BTreeSet, HashMap, HashSet};

use docxtpl_opc::{
    relationships_path_of, resolve_part_target, OpcError, Package, PartUri, Relationship,
    Relationships, TargetMode,
};
use docxtpl_rich::{probe, InlineImage};
use docxtpl_template::{ImageRegistry, ImageRels, ImageResolveError};
use sha1::{Digest, Sha1};

/// image 关系类型。
const IMAGE_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";

/// hyperlink 外部关系类型（`tpl.build_url_id` 与图片锚点共用）。
const HYPERLINK_REL_TYPE: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/hyperlink";

/// 渲染期暂存的图片/关系/Content Types 变更。
pub(crate) struct ImageInjections {
    /// 主文档 part 名（如 `word/document.xml`）。
    main_name: String,
    /// 主文档 rels part 名（如 `word/_rels/document.xml.rels`）。
    rels_name: String,
    /// 主文档关系（从模板克隆，新增条目尾插）。
    rels: Relationships,
    /// rels 是否被改过（决定是否回写）。
    rels_dirty: bool,
    /// 已占用的 imageN 编号集合（含模板既有图片）。
    used_numbers: BTreeSet<u64>,
    /// sha1 → 绝对 part 名（模板既有 + 本次新增）。
    by_sha1: HashMap<String, String>,
    /// 待新增 part：（绝对 part 名，字节）。
    pending_parts: Vec<(String, Vec<u8>)>,
    /// 待新增 Content Types Default：（小写扩展名, content type）。
    pending_defaults: Vec<(String, String)>,
    /// 模板 CT 已有的 Default 扩展名（小写）。
    known_defaults: HashSet<String>,
}

impl ImageInjections {
    /// 从已打开的包构造：克隆主文档 rels，DFS 收集既有图片 part 编号与 sha1。
    pub(crate) fn new(pkg: &Package, main_name: &str) -> Result<Self, OpcError> {
        let main_uri = PartUri::new(main_name)?;
        let rels_name = relationships_path_of(&main_uri);
        let rels = pkg
            .relationships_of(main_name)
            .cloned()
            .ok_or_else(|| OpcError::Malformed {
                reason: format!("主文档缺少关系文件 {rels_name}"),
            })?;

        let existing_images = collect_image_parts(pkg);
        let mut used_numbers = BTreeSet::new();
        let mut by_sha1 = HashMap::new();
        for name in &existing_images {
            if let Some(part) = pkg.part(name) {
                let digest = hex_sha1(part.bytes());
                // 同一字节若被多个 part 名引用，保留首个（dict 插入序）。
                by_sha1.entry(digest).or_insert_with(|| name.clone());
                if let Some(number) = image_number(name) {
                    used_numbers.insert(number);
                }
            }
        }

        let known_defaults = pkg
            .content_types()
            .defaults()
            .map(|(ext, _)| ext.to_string())
            .collect();

        Ok(Self {
            main_name: main_name.to_string(),
            rels_name,
            rels,
            rels_dirty: false,
            used_numbers,
            by_sha1,
            pending_parts: Vec::new(),
            pending_defaults: Vec::new(),
            known_defaults,
        })
    }

    /// 上游 `DocxTemplate.build_url_id`：在渲染前登记外部超链接关系。
    ///
    /// 同 URL（reltype+External）复用既有 rId；否则尾插新关系并回填 Id 空洞。
    pub(crate) fn build_url_id(&mut self, url: &str) -> String {
        self.get_or_add(HYPERLINK_REL_TYPE, url, TargetMode::External)
    }

    /// 分配下一个图片编号：1 起回填空洞，否则最大值 +1
    /// （等价上游 `range(1, len+1)` 回填空洞的结果）。
    fn next_image_number(&self) -> u64 {
        let mut number = 1;
        loop {
            if !self.used_numbers.contains(&number) {
                return number;
            }
            number += 1;
        }
    }

    /// 上游 `_Relationships.get_or_add` / `get_or_add_ext_rel`。
    fn get_or_add(&mut self, rel_type: &str, target: &str, mode: TargetMode) -> String {
        if let Some(rel) = self.rels.find_matching(rel_type, target, mode) {
            return rel.id.clone();
        }
        let id = self.rels.next_r_id();
        self.rels.push(Relationship {
            id: id.clone(),
            rel_type: rel_type.to_string(),
            target: target.to_string(),
            target_mode: mode,
        });
        self.rels_dirty = true;
        id
    }

    /// 把暂存变更写入包：先加 media part（消除悬空关系），再回写 rels，
    /// 最后重建 [Content_Types].xml（保证 validate 能查到新 part 类型）。
    pub(crate) fn apply(&mut self, pkg: &mut Package) -> Result<(), OpcError> {
        for (name, bytes) in self.pending_parts.drain(..) {
            pkg.add_part(&name, bytes)?;
        }
        if self.rels_dirty {
            let rels_xml = self.rels.to_xml();
            pkg.set_part_bytes(&self.rels_name, rels_xml.into_bytes())?;
        }
        if !self.pending_defaults.is_empty() {
            let mut content_types = pkg.content_types().clone();
            for (extension, content_type) in &self.pending_defaults {
                content_types.add_default(extension, content_type);
            }
            pkg.set_part_bytes("[Content_Types].xml", content_types.to_xml().into_bytes())?;
        }
        Ok(())
    }
}

impl ImageRegistry for ImageInjections {
    fn resolve_image(&mut self, image: &InlineImage) -> Result<ImageRels, ImageResolveError> {
        // 1. probe：上游 get_or_add_image 内的图片头解析，坏图在此抛
        //    UnrecognizedImageError（在任何 part/rId 分配之前）。
        let info = probe(&image.blob).map_err(|err| ImageResolveError {
            message: err.to_string(),
        })?;

        // 2. sha1 去重或分配新 part 名（编号跨扩展名、回填空洞）。
        let target_abs = match self.by_sha1.get(&info.sha1) {
            Some(existing) => existing.clone(),
            None => {
                let number = self.next_image_number();
                let name = format!("word/media/image{number}.{}", info.ext);
                self.used_numbers.insert(number);
                self.by_sha1.insert(info.sha1.clone(), name.clone());
                self.pending_parts.push((name.clone(), image.blob.clone()));
                if !self.known_defaults.contains(info.ext)
                    && !self
                        .pending_defaults
                        .iter()
                        .any(|(ext, _)| ext.as_str() == info.ext)
                {
                    self.pending_defaults
                        .push((info.ext.to_string(), info.content_type.to_string()));
                }
                name
            }
        };

        // 3. 主文档 rels：图片内部关系（Target 为相对主文档目录的路径）。
        let rel_target = relative_to_owner(&self.main_name, &target_abs);
        let blip_rid = self.get_or_add(IMAGE_REL_TYPE, &rel_target, TargetMode::Internal);

        // 4. 锚点外链：上游在图片 rId 之后分配。
        let hyperlink_rid = image
            .anchor
            .as_ref()
            .map(|url| self.get_or_add(HYPERLINK_REL_TYPE, url, TargetMode::External));

        Ok(ImageRels {
            blip_rid,
            hyperlink_rid,
        })
    }
}

/// 全包 DFS 收集所有 image 类型**内部**关系的目标绝对 part 名
/// （对齐 `OpcPackage.iter_rels`：从根 rels 起，external 跳过，每个 part
/// 的 rels 只遍历一次；image 关系按出现去重，与遍历顺序无关）。
fn collect_image_parts(pkg: &Package) -> Vec<String> {
    let mut visited: HashSet<String> = HashSet::new();
    let mut images: Vec<String> = Vec::new();
    // 栈元素：（owner part 所在目录，rels）。根 rels 的 base 为 None。
    let mut stack: Vec<(Option<String>, Relationships)> =
        vec![(None, pkg.root_relationships().clone())];

    while let Some((base_dir, rels)) = stack.pop() {
        let base_uri = base_dir.as_deref().and_then(|dir| PartUri::new(dir).ok());
        for rel in rels.iter() {
            if rel.target_mode != TargetMode::Internal {
                continue;
            }
            let Some(target) = resolve_part_target(base_uri.as_ref(), &rel.target) else {
                continue;
            };
            let name = target.as_str().to_string();
            if rel.rel_type == IMAGE_REL_TYPE && !images.contains(&name) {
                images.push(name.clone());
            }
            // 每个 part 的 rels 只入栈一次（visited 去重）。
            if visited.insert(name.clone()) {
                if let Some(part) = pkg.part(&name) {
                    if let Some(child_rels) = part.relationships() {
                        stack.push((parent_dir(&name), child_rels.clone()));
                    }
                }
            }
        }
    }
    images
}

/// 取 part 名的所在目录（`word/document.xml` → `word`；包根 → `None`）。
fn parent_dir(name: &str) -> Option<String> {
    name.rsplit_once('/')
        .and_then(|(dir, _)| (!dir.is_empty()).then(|| dir.to_string()))
}

/// 计算目标绝对 part 相对 owner part 所在目录的引用路径
/// （posix relpath：`word/document.xml` + `word/media/i.png` →
/// `media/i.png`；需要上溯时产出 `..`）。
fn relative_to_owner(owner_part: &str, target_abs: &str) -> String {
    fn segments(name: &str) -> Vec<&str> {
        name.split('/')
            .filter(|segment| !segment.is_empty())
            .collect()
    }
    let mut base = segments(owner_part);
    base.pop(); // 去掉 owner 文件名，剩其目录段
    let target = segments(target_abs);

    let mut common = 0;
    while common < base.len() && common < target.len() && base[common] == target[common] {
        common += 1;
    }
    let mut out: Vec<&str> = (0..base.len() - common).map(|_| "..").collect();
    out.extend(target[common..].iter().copied());
    out.join("/")
}

/// 上游 PackURI.idx 正则 `^([a-zA-Z]+)([1-9][0-9]*)?`：
/// 文件名字母前缀后的数字后缀（首数字不能为 0），无则 None。
fn image_number(abs_name: &str) -> Option<u64> {
    let file_name = abs_name.rsplit('/').next().unwrap_or(abs_name);
    let stem = file_name
        .rsplit_once('.')
        .map_or(file_name, |(stem, _)| stem);
    let bytes = stem.as_bytes();
    let digit_at = bytes.iter().position(u8::is_ascii_digit)?;
    // 前缀必须是 1+ 个纯字母。
    if digit_at == 0 || !bytes[..digit_at].iter().all(u8::is_ascii_alphabetic) {
        return None;
    }
    // 数字段首位必须 1-9（[1-9][0-9]*）。
    if bytes[digit_at] == b'0' {
        return None;
    }
    let digit_end = bytes[digit_at..]
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .map_or(bytes.len(), |offset| digit_at + offset);
    stem[digit_at..digit_end].parse().ok()
}

/// SHA-1 小写十六进制（对齐 `hashlib.sha1(blob).hexdigest()`）。
fn hex_sha1(bytes: &[u8]) -> String {
    let mut hasher = Sha1::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_number_matches_packuri_idx_regex() {
        assert_eq!(image_number("word/media/image1.png"), Some(1));
        assert_eq!(image_number("word/media/image12.jpg"), Some(12));
        assert_eq!(image_number("media/image2.bmp"), Some(2));
        // 0 开头不匹配 [1-9]
        assert_eq!(image_number("media/image01.png"), None);
        // 无数字后缀
        assert_eq!(image_number("media/image.png"), None);
        // 非字母前缀
        assert_eq!(image_number("media/1image.png"), None);
        // re.match 不锚尾：字母前缀+数字开头即可取到数字
        assert_eq!(image_number("media/image3x.png"), Some(3));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(
            relative_to_owner("word/document.xml", "word/media/image1.png"),
            "media/image1.png"
        );
        assert_eq!(
            relative_to_owner("word/document.xml", "word/media/sub/i.png"),
            "media/sub/i.png"
        );
        assert_eq!(
            relative_to_owner("word/sub/document.xml", "word/media/i.png"),
            "../media/i.png"
        );
        assert_eq!(
            relative_to_owner("document.xml", "media/i.png"),
            "media/i.png"
        );
    }

    #[test]
    fn parent_dirs() {
        assert_eq!(parent_dir("word/document.xml").as_deref(), Some("word"));
        assert_eq!(parent_dir("document.xml"), None);
        assert_eq!(
            parent_dir("word/media/i.png").as_deref(),
            Some("word/media")
        );
    }
}

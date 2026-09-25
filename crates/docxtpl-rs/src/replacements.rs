//! P7 媒体/嵌入替换族（ADR-008）：对齐 docxtpl 0.20.2 save 链路上的
//! 两条独立替换路径。
//!
//! **路径 A：[`Replacements::apply_pic_replacements`]（pre_processing，
//! template.py L788–878）**——`replace_pic`。在 python-docx 全量保存
//! **之前**扫描主文档与主 rels 中每个 HEADER/FOOTER part 的
//! `//a:graphic/a:graphicData[@uri = pic]`，按 cNvPr 的
//! name/title/descr 匹配注册标识，命中后直接换 r:embed 目标 part 的
//! blob（随后随全量保存落盘；part 名、CT、rels、wp:extent 均不变）。
//! 任一注册标识全程未命中 → `ValueError`（[`TemplateErrorKind::InvalidArgument`]）。
//!
//! **路径 B：[`Replacements::apply_byte_replacements`]（post_processing，
//! template.py L749–786）**——`replace_media` / `replace_embedded` /
//! `replace_zipname`。在包最终写出阶段逐条目按
//! `zipname 精确命中 > word/media/ 前缀且 CRC32 命中 >
//! word/embeddings/ 前缀且 CRC32 命中` 的优先级换字节；不解析 XML、
//! 不改 part 名/Content Types/关系。
//!
//! 注册只暂存字节，不做 I/O；同一 [`crate::RenderSession`] 内可多次
//! 注册，[`Replacements::reset`] 对齐上游 `reset_replacements`。

use std::collections::HashMap;

use docxtpl_opc::{resolve_part_target, Package, PartUri, TargetMode};
use docxtpl_template::{RenderError, TemplateErrorKind};
use docxtpl_xml::{ns_uri, NodeId, XmlDocument, XmlLimits};

use crate::Error;

/// image 关系类型（r:embed 目标）。
const RT_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
/// header 关系类型（上游比较 `rel.reltype == REL_TYPE.HEADER`）。
const RT_HEADER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
/// footer 关系类型。
const RT_FOOTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";

/// zip 条目 media 前缀（zipname 不带前导 `/`）。
const MEDIA_PREFIX: &str = "word/media/";
/// zip 条目 embeddings 前缀。
const EMBEDDINGS_PREFIX: &str = "word/embeddings/";

/// 单个 replace_pic 注册项：新字节 + 是否已在扫描中命中。
struct PicReplacement {
    /// 替换图片字节（对齐上游 `pics_to_replace[img_id]`）。
    bytes: Vec<u8>,
    /// 对应 `_replace_pics` 的 replaced_pics 命中标记。
    hit: bool,
}

/// P7 替换族注册表（会话级，随 [`crate::RenderSession`] 持有）。
pub(crate) struct Replacements {
    /// CRC32(源字节) → 新字节（`word/media/` 条目按 zip CRC 命中）。
    media: HashMap<u32, Vec<u8>>,
    /// CRC32(源字节) → 新字节（`word/embeddings/` 条目）。
    embedded: HashMap<u32, Vec<u8>>,
    /// zip 条目全名（如 `word/embeddings/x.bin`）→ 新字节。
    zipnames: HashMap<String, Vec<u8>>,
    /// 图片标识（cNvPr name/title/descr）→ 替换项。
    ///
    /// 保序 Vec 而非 map：上游 `pics_to_replace` 是 dict，每个图片
    /// graphicData 按**注册插入序**找首个匹配且 `break`（先注册的 key
    /// 会遮蔽同图上的后注册 key），报错顺序也按插入序。
    pics: Vec<(String, PicReplacement)>,
}

impl Replacements {
    pub(crate) fn new() -> Self {
        Self {
            media: HashMap::new(),
            embedded: HashMap::new(),
            zipnames: HashMap::new(),
            pics: Vec::new(),
        }
    }

    /// 上游 `replace_media(src_file, dst_file)`：按源字节 CRC32 注册
    /// media 替换（源/目标均为已读入内存的字节）。
    pub(crate) fn replace_media(&mut self, src: &[u8], dst: &[u8]) {
        self.media.insert(crc32fast::hash(src), dst.to_vec());
    }

    /// 上游 `replace_embedded(src_file, dst_file)`：按源字节 CRC32 注册
    /// embeddings 替换。
    pub(crate) fn replace_embedded(&mut self, src: &[u8], dst: &[u8]) {
        self.embedded.insert(crc32fast::hash(src), dst.to_vec());
    }

    /// 上游 `replace_zipname(zipname, dst_file)`：按 zip 条目全名精确注册。
    ///
    /// `zipname` 为包内条目名（不带前导 `/`，如
    /// `word/embeddings/Feuille1.xlsx`）。
    pub(crate) fn replace_zipname(&mut self, zipname: &str, dst: &[u8]) {
        self.zipnames.insert(zipname.to_string(), dst.to_vec());
    }

    /// 上游 `replace_pic(embedded_file, dst_file)`：按 cNvPr
    /// name/title/descr 注册图片替换。重复注册同一标识按 dict 语义
    /// 覆盖新字节（保留首次注册位置）。
    pub(crate) fn replace_pic(&mut self, pic_id: &str, dst: &[u8]) {
        if let Some(slot) = self.pics.iter_mut().find(|(id, _)| id == pic_id) {
            slot.1.bytes = dst.to_vec();
            return;
        }
        self.pics.push((
            pic_id.to_string(),
            PicReplacement {
                bytes: dst.to_vec(),
                hit: false,
            },
        ));
    }

    /// 上游 `reset_replacements`：清空全部四类替换注册。
    pub(crate) fn reset(&mut self) {
        self.media.clear();
        self.embedded.clear();
        self.zipnames.clear();
        self.pics.clear();
    }

    /// 路径 A：替换主文档与全部 HEADER/FOOTER part 中的图片 part blob。
    ///
    /// 必须在全部渲染/图片注入落定**之后**、Content Types 归一之前调用
    /// （对齐上游 save：pre_processing 在 docx.save 之前；此处扫描的是
    /// 渲染后的最终 XML）。
    pub(crate) fn apply_pic_replacements(
        &mut self,
        pkg: &mut Package,
        main_name: &str,
    ) -> Result<(), Error> {
        if self.pics.is_empty() {
            return Ok(());
        }

        // Main document（上游 `part = self.docx.part`）。
        self.replace_pics_in_part(pkg, main_name)?;

        // Header/Footer：主 rels 两遍（先 header 后 footer），按 rels
        // 出现序、不去重（重复扫描同 part 幂等；上游同样不去重）。
        for part_name in header_footer_targets(pkg, main_name) {
            self.replace_pics_in_part(pkg, &part_name)?;
        }

        // 对齐上游 allow_missing_pics=False（默认）：任一标识未命中即报错
        // （按注册插入序，与上游 dict 迭代一致）。
        for (img_id, pic) in &self.pics {
            if !pic.hit {
                return Err(Error::Render(RenderError::Template {
                    kind: TemplateErrorKind::InvalidArgument,
                    part: main_name.to_string(),
                    line: None,
                    message: format!("Picture {img_id} not found in the docx template"),
                    context: Vec::new(),
                }));
            }
        }
        Ok(())
    }

    /// 路径 B：遍历最终包 part，按 zipname/media CRC/embeddings CRC
    /// 优先级替换字节。应在 Content Types 归一之后、最终校验之前调用。
    pub(crate) fn apply_byte_replacements(&mut self, pkg: &mut Package) -> Result<(), Error> {
        if self.media.is_empty() && self.embedded.is_empty() && self.zipnames.is_empty() {
            return Ok(());
        }

        // 迭代与写回分离（pkg.parts() 持不可变借用）。
        let mut hits: Vec<(String, Vec<u8>)> = Vec::new();
        for part in pkg.parts() {
            let abs_name = part.uri().as_str();
            let zipname = abs_name.strip_prefix('/').unwrap_or(abs_name);
            if let Some(bytes) = self.zipnames.get(zipname) {
                hits.push((abs_name.to_string(), bytes.clone()));
            } else if zipname.starts_with(MEDIA_PREFIX) {
                let crc = crc32fast::hash(part.bytes());
                if let Some(bytes) = self.media.get(&crc) {
                    hits.push((abs_name.to_string(), bytes.clone()));
                }
            } else if zipname.starts_with(EMBEDDINGS_PREFIX) {
                let crc = crc32fast::hash(part.bytes());
                if let Some(bytes) = self.embedded.get(&crc) {
                    hits.push((abs_name.to_string(), bytes.clone()));
                }
            }
        }

        for (name, bytes) in hits {
            pkg.set_part_bytes(&name, bytes)?;
        }
        Ok(())
    }

    /// 对齐上游 `_replace_docx_part_pics`：扫描单个文档/story part 的
    /// pic graphicData 并替换其 r:embed 目标 part 的 blob。
    fn replace_pics_in_part(&mut self, pkg: &mut Package, owner: &str) -> Result<(), Error> {
        let Some(part) = pkg.part(owner) else {
            return Ok(());
        };
        // 渲染管线输出的 XML 必为 UTF-8 文本。
        let xml = std::str::from_utf8(part.bytes())
            .map(str::to_owned)
            .map_err(|source| Error::NotUtf8 {
                part: owner.to_string(),
                source,
            })?;
        let rels = pkg.relationships_of(owner).cloned();
        let base_dir = PartUri::new(owner).ok().and_then(|uri| uri.parent());

        // 上游 etree.fromstring(doc_part.blob)：严格解析；该 blob 来自
        // 自家渲染/序列化管线，良构是不变量，失败按 XML 错误上报。
        let doc = XmlDocument::parse_strict(&xml, &XmlLimits::default()).map_err(|source| {
            Error::Render(RenderError::Xml {
                part: owner.to_string(),
                source,
            })
        })?;

        // xpath //a:graphic/a:graphicData：后代 a:graphic 的直接 a:graphicData 子。
        let mut pending: Vec<(String, Vec<u8>)> = Vec::new();
        for graphic in tag_descendants(&doc, doc.root(), ns_uri::A, "graphic") {
            let Some(gd) = direct_child(&doc, graphic, ns_uri::A, "graphicData") else {
                continue;
            };
            process_graphic_data(
                &doc,
                gd,
                rels.as_ref(),
                base_dir.as_ref(),
                &mut self.pics,
                &mut pending,
            );
        }

        // 命中的 media part 换 blob（同一 part 多 blip 引用只写一份，
        // 重复写入同字节亦幂等）。
        for (target, bytes) in pending {
            if pkg.contains(&target) {
                pkg.set_part_bytes(&target, bytes)?;
            }
        }
        Ok(())
    }
}

/// 处理单个 a:graphicData（对齐上游 per-gd try/except：任何结构缺失
/// （无 uri 属性、无 blip、无 cNvPr@name、悬空 r:id 等）都整体跳过）。
#[allow(clippy::too_many_arguments)]
fn process_graphic_data(
    doc: &XmlDocument,
    gd: NodeId,
    rels: Option<&docxtpl_opc::Relationships>,
    base_dir: Option<&PartUri>,
    pics: &mut [(String, PicReplacement)],
    pending: &mut Vec<(String, Vec<u8>)>,
) {
    // 仅 pic:pic 形态的 graphicData（其他为 chart/SmartArt 等，continue）。
    if doc.attr(gd, "", "uri") != Some(ns_uri::PIC) {
        return;
    }

    // pic:pic/pic:blipFill/a:blip：无 blip（xpath[0] 越界）→ 跳过。
    let Some(pic) = direct_child(doc, gd, ns_uri::PIC, "pic") else {
        return;
    };
    let Some(blip_fill) = direct_child(doc, pic, ns_uri::PIC, "blipFill") else {
        return;
    };
    let Some(blip) = direct_child(doc, blip_fill, ns_uri::A, "blip") else {
        return;
    };
    // 只有 r:link 无 r:embed（LINKED_PICTURE）→ 上游 continue。
    let Some(embed_rid) = doc.attr(blip, ns_uri::R, "embed") else {
        return;
    };

    // pic:pic/pic:nvPicPr/pic:cNvPr 的 name/title/descr（name 缺失时
    // 上游 xpath[0] 越界被吞掉）。
    let Some(nvpicpr) = direct_child(doc, pic, ns_uri::PIC, "nvPicPr") else {
        return;
    };
    let Some(cnvpr) = direct_child(doc, nvpicpr, ns_uri::PIC, "cNvPr") else {
        return;
    };
    let Some(filename) = doc.attr(cnvpr, "", "name") else {
        return;
    };
    let title = doc.attr(cnvpr, "", "title").unwrap_or("");
    let description = doc.attr(cnvpr, "", "descr").unwrap_or("");

    // doc_part.rels[r:embed]：仅内部关系、目标可解析且 part 存在；
    // 外链/悬空 r:id 在上游抛异常被吞掉。
    let Some(rels) = rels else {
        return;
    };
    let Some(rel) = rels.iter().find(|rel| {
        rel.id == embed_rid && rel.target_mode == TargetMode::Internal && rel.rel_type == RT_IMAGE
    }) else {
        return;
    };
    let Some(target_uri) = resolve_part_target(base_dir, &rel.target) else {
        return;
    };
    let target = target_uri.as_str().to_string();

    // 标识等于 name/title/descr 任一即命中；按 pics 注册插入序比较，
    // 命中后 break（一个 gd 只换一次）。
    for (img_id, pic_repl) in pics.iter_mut() {
        if img_id == filename || img_id == title || img_id == description {
            pending.push((target.clone(), pic_repl.bytes.clone()));
            pic_repl.hit = true;
            break;
        }
    }
}

/// 主文档 rels 中 HEADER/FOOTER 内部目标 part 名：先 header 后 footer
/// 两遍、按 rels 出现序、不去重（对齐 `_replace_pics` 的 parts 收集）。
fn header_footer_targets(pkg: &Package, main_name: &str) -> Vec<String> {
    let Ok(main_uri) = PartUri::new(main_name) else {
        return Vec::new();
    };
    let base_dir = main_uri.parent();
    let Some(rels) = pkg.relationships_of(main_name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for rel_type in [RT_HEADER, RT_FOOTER] {
        for rel in rels.iter() {
            if rel.target_mode != TargetMode::Internal || rel.rel_type != rel_type {
                continue;
            }
            if let Some(target) = resolve_part_target(base_dir.as_ref(), &rel.target) {
                let name = target.as_str();
                // python-docx rels 与包 parts 图一致；目标缺失属畸形包，跳过。
                if pkg.part(name).is_some() {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// 节点是否为指定限定名元素。
fn is_tag(doc: &XmlDocument, id: NodeId, ns: &str, local: &str) -> bool {
    doc.tag(id).is_some_and(|q| q.ns == ns && q.local == local)
}

/// XPath descendant 轴（不含起始节点自身）下指定限定名的节点（文档序）。
fn tag_descendants(doc: &XmlDocument, root: NodeId, ns: &str, local: &str) -> Vec<NodeId> {
    doc.descendants(root)
        .into_iter()
        .skip(1)
        .filter(|&id| is_tag(doc, id, ns, local))
        .collect()
}

/// 元素的直接子中第一个指定限定名节点。
fn direct_child(doc: &XmlDocument, parent: NodeId, ns: &str, local: &str) -> Option<NodeId> {
    doc.children(parent)
        .iter()
        .copied()
        .find(|&id| is_tag(doc, id, ns, local))
}

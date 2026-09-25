//! P6 Subdoc 子文档合并（ADR-007）：`tpl.new_subdoc(docpath)` 的等价实现。
//!
//! 上游链路：docxtpl `Template.new_subdoc` → `Subdoc(tpl, docpath)` 构造期
//! 执行 `SubdocComposer.attach_parts`（docxtpl/subdoc.py，语义源自
//! docxcompose 2.2.0 的 `Composer`），把外部 docx 的部件**合并进主文档**：
//! body 片段留给 jinja 注入（`{{p sd }}` 模板位），而样式/编号/关系/
//! media 等 part 直接变更主包。本模块按同一编排逐等价实现：
//!
//! 1. 打开 sub 包，解析其 document/styles/numbering/footnotes 树
//!    （对齐 python-docx oxml 解析器 `remove_blank_text`：全部剥空白）；
//! 2. 逐 sub body 直接子级（跳过 `w:sectPr`）：引用部件复制
//!    （add_referenced_parts，含 add_relationship 的递归整图复制）→
//!    样式三分支合并（add_styles）→ 编号复制（add_numberings）→
//!    列表编号重启检测（restart_first_numbering，实际触发视为主包
//!    不支持）→ 图片合并（add_images）→ SmartArt/VML/脚注检测
//!    （不支持，主包无对应部件语义）→ 删除页眉页脚引用；
//! 3. sub footnotes 中的样式合并（add_styles_from_other_parts）；
//! 4. 主文档 bookmark/docPr/cNvPr 重编号（renumber 三兄弟，作用于
//!    主包正文与页眉页脚部件，对齐上游序：bookmarkStart 与 bookmarkEnd
//!    各自从 0 计数、docPr/cNvPr 正文后跨 part 续编）；
//! 5. 分节类型修正（fix_section_types：两侧均多节时需要改主分节起始
//!    类型，视为主包不支持）；
//! 6. 生成片段：移除 body 直接 sectPr 后，其余子级顺序拼接、无命名空间
//!    声明（上游 `_get_xml` 剥 body 标签时提升声明一并丢失），
//!    以 [`RenderValue::Subdoc`] 返回。
//!
//! 落盘路径：主 document/styles/numbering 与页眉页脚 part 仅在树被实际
//! 修改时按 python-docx 序列化形态写回（dirty 门控，未改字节原样保留）；
//! 复制的非图片 part 与其 rels、Content Types 变更即时落包；图片与主
//! 文档 rels 走 [`ImageInjections`] 暂存、由 `RenderSession::finish`
//! 统一落定（渲染期图片共用同一编号/去重状态）。

use std::collections::{HashMap, HashSet};
use std::path::Path;

use docxtpl_opc::{
    relationships_path_of, resolve_part_target, ContentTypes, OpcError, Package, PackageLimits,
    PartUri, Relationship, Relationships, TargetMode,
};
use docxtpl_template::RenderValue;
use docxtpl_xml::{ns_uri, NodeId, XmlDocument, XmlLimits};

use crate::images::{relative_to_owner, ImageInjections};
use crate::Error;

/// image 关系类型（与 images.rs 一致）。
const RT_IMAGE: &str = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/image";
/// header 关系类型。
const RT_HEADER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/header";
/// footer 关系类型。
const RT_FOOTER: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footer";
/// footnotes 关系类型。
const RT_FOOTNOTES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/footnotes";
/// styles 关系类型。
const RT_STYLES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles";
/// numbering 关系类型。
const RT_NUMBERING: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
/// custom-properties 关系类型。
const RT_CUSTOM_PROPERTIES: &str =
    "http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties";

/// rels part 的 content type（新建 `.rels` part 时登记）。
const CT_RELS: &str = "application/vnd.openxmlformats-package.relationships+xml";

/// `asvg` 前缀（SVG 扩展 blip，上游 add_images 的 `asvg:svgBlip`）。
const NS_ASVG: &str = "http://schemas.microsoft.com/office/drawing/2016/SVG/main";
/// `dgm` 前缀（SmartArt 关系引用）。
const NS_DGM: &str = "http://schemas.openxmlformats.org/officeDocument/2006/diagram";

/// `tpl.new_subdoc(docpath)` 入口：打开外部 docx 并执行部件合并，
/// 返回可直接 `ctx.insert("sd", ...)` 的 Subdoc 片段值。
pub(crate) fn new_subdoc(
    pkg: &mut Package,
    injections: &mut ImageInjections,
    docpath: &Path,
) -> Result<RenderValue, Error> {
    // 上游 `Document(docpath)`：畸形 docx 直接失败。
    let sub_pkg = Package::open(docpath, &PackageLimits::default())?;
    sub_pkg.validate()?;
    let mut ctx = SubdocComposer::new(pkg, injections, sub_pkg)?;
    ctx.attach_parts()
}

/// 一次 Subdoc 合并的全部状态（对齐上游 `SubdocComposer` 实例状态：
/// mapping 在 attach_parts 开头 reset 一次、全程共享）。
struct SubdocComposer<'a> {
    pkg: &'a mut Package,
    injections: &'a mut ImageInjections,
    /// sub 包（owned；读取其 part/rels/Content Types）。
    sub_pkg: Package,
    /// 主文档 part 名（如 `word/document.xml`）。
    main_name: String,
    /// sub 文档 part 名。
    sub_main_name: String,
    /// sub 主文档 part 的关系集合（上游 `doc.part.rels`）。
    sub_rels: Relationships,
    // ---- 编号映射（上游 reset_reference_mapping）----
    /// sub numId → 主 numId。
    num_id_mapping: HashMap<i64, i64>,
    /// sub abstractNumId → 主 abstractNumId。
    anum_id_mapping: HashMap<i64, i64>,
    /// 已重启编号的样式 id（上游 `self._numbering_restarted`）。
    numbering_restarted: HashSet<String>,
    // ---- 样式映射（上游 _create_style_id_mapping）----
    /// sub 样式 id → name。
    style_id2name: HashMap<String, String>,
    /// 主样式 name → id。
    style_name2id: HashMap<String, String>,
    // ---- 树（全部预载，等价 python-docx 打开即建树并剥空白）----
    sub_doc: XmlDocument,
    sub_styles: XmlDocument,
    /// sub footnotes part 树（缺失为 None，等价上游 KeyError → pass）。
    sub_footnotes: Option<XmlDocument>,
    /// sub numbering part 树（缺失为 None：上游按需新建空部件，查询无命中）。
    sub_numbering: Option<XmlDocument>,
    main_doc: XmlDocument,
    main_styles: XmlDocument,
    /// 主 numbering part 树（缺失为 None：需要时上游从内置模板新建，本实现
    /// 视为主包不支持并报错）。
    main_numbering: Option<XmlDocument>,
    /// 页眉页脚 part 树缓存（docPr/cNvPr 重编号；同 part 多关系只解析一次）。
    hf_trees: HashMap<String, XmlDocument>,
    // ---- part 名 ----
    main_styles_name: String,
    main_numbering_name: Option<String>,
    // ---- Content Types 变更（python-docx 保存时从 parts 全量重建，本实现
    //      克隆后修改、结束时一次性写回）----
    content_types: ContentTypes,
    ct_dirty: bool,
    // ---- dirty 门控：树未变则 part 字节原样保留 ----
    main_doc_dirty: bool,
    main_styles_dirty: bool,
    main_numbering_dirty: bool,
    dirty_hf: HashSet<String>,
}

impl<'a> SubdocComposer<'a> {
    fn new(
        pkg: &'a mut Package,
        injections: &'a mut ImageInjections,
        sub_pkg: Package,
    ) -> Result<Self, Error> {
        let main_name = pkg.main_document_uri()?.as_str().to_string();
        let sub_main_name = sub_pkg.main_document_uri()?.as_str().to_string();

        // sub 主文档必须有 rels 文件（python-docx `doc.part.rels` 惰性空表
        // 与标准 docx 不符，畸形包直接失败）。
        let sub_rels = sub_pkg
            .relationships_of(&sub_main_name)
            .cloned()
            .ok_or_else(|| malformed(format!("子文档缺少关系文件（part {sub_main_name}）")))?;

        // 树预载：python-docx 打开后所有 XML part 都经 remove_blank_text
        // 解析，故序列化（落盘/渲染输入）一律为剥空白形态。
        let main_doc = load_xml_tree(pkg, &main_name)?;
        let sub_doc = load_xml_tree(&sub_pkg, &sub_main_name)?;

        // 主缺 styles.xml：python-docx `doc.styles` 直接 KeyError，视为主包不支持。
        let main_styles_name = related_part(pkg, &main_name, RT_STYLES).ok_or_else(|| {
            malformed("主文档缺少样式部件（word/styles.xml），无法合并子文档样式")
        })?;
        let main_styles = load_xml_tree(pkg, &main_styles_name)?;

        // sub 缺 styles.xml：python-docx 同样 KeyError，子文档视为畸形。
        let sub_styles_name = related_part(&sub_pkg, &sub_main_name, RT_STYLES)
            .ok_or_else(|| malformed("子文档缺少样式部件（word/styles.xml）"))?;
        let sub_styles = load_xml_tree(&sub_pkg, &sub_styles_name)?;

        let main_numbering_name = related_part(pkg, &main_name, RT_NUMBERING);
        let main_numbering = match &main_numbering_name {
            Some(name) => Some(load_xml_tree(pkg, name)?),
            None => None,
        };
        let sub_numbering = related_part(&sub_pkg, &sub_main_name, RT_NUMBERING)
            .map(|name| load_xml_tree(&sub_pkg, &name))
            .transpose()?;
        let sub_footnotes = related_part(&sub_pkg, &sub_main_name, RT_FOOTNOTES)
            .map(|name| load_xml_tree(&sub_pkg, &name))
            .transpose()?;

        // 上游 `_create_style_id_mapping`：样式 id 按语言而 name 相对稳定，
        // 以 name 为桥把 sub 的 id 映射到主文档的同名样式 id。
        let style_id2name = style_id_name_map(&sub_styles);
        let style_name2id = style_name_id_map(&main_styles);

        // Content Types 克隆后修改、结束时一次性写回（python-docx 保存时
        // 从 parts 全量重建，此处等价维护同一份声明表）。
        let content_types = pkg.content_types().clone();

        Ok(Self {
            pkg,
            injections,
            sub_pkg,
            main_name,
            sub_main_name,
            sub_rels,
            num_id_mapping: HashMap::new(),
            anum_id_mapping: HashMap::new(),
            numbering_restarted: HashSet::new(),
            style_id2name,
            style_name2id,
            sub_doc,
            sub_styles,
            sub_footnotes,
            sub_numbering,
            main_doc,
            main_styles,
            main_numbering,
            hf_trees: HashMap::new(),
            main_styles_name,
            main_numbering_name,
            content_types,
            ct_dirty: false,
            main_doc_dirty: false,
            main_styles_dirty: false,
            main_numbering_dirty: false,
            dirty_hf: HashSet::new(),
        })
    }

    /// 上游 `SubdocComposer.attach_parts` 编排（docxtpl/subdoc.py）。
    fn attach_parts(&mut self) -> Result<RenderValue, Error> {
        // CustomProperties dissolve：sub 有 custom.xml 时上游会改主包核心
        // 属性并溶解域，本实现视为主包不支持。
        if related_part(&self.sub_pkg, &self.sub_main_name, RT_CUSTOM_PROPERTIES).is_some() {
            return Err(malformed(
                "子文档含自定义属性部件（docProps/custom.xml），dissolve_fields 不支持",
            ));
        }

        // 逐 sub body 直接子级（跳过 w:sectPr），顺序对齐上游。
        let sub_body = body_of(&self.sub_doc)?;
        let elements: Vec<NodeId> = self.sub_doc.children(sub_body).to_vec();
        for element in elements {
            if is_tag(&self.sub_doc, element, ns_uri::W, "sectPr") {
                continue;
            }
            self.add_referenced_parts(element)?;
            self.add_styles_in_subdoc(element)?;
            self.add_numberings_in_subdoc(element)?;
            self.restart_first_numbering(element)?;
            self.add_images(element)?;
            self.check_diagrams(element)?;
            self.check_shapes(element)?;
            self.check_footnotes(element)?;
            self.remove_header_and_footer_references(element);
        }

        // 循环后尾段（顺序固定）。
        self.add_styles_from_other_parts()?;
        self.renumber_bookmarks();
        self.renumber_ids(ns_uri::WP, "docPr")?;
        self.renumber_ids(ns_uri::PIC, "cNvPr")?;
        self.fix_section_types()?;

        // 主树写回（dirty 门控）与 Content Types 落盘。
        self.flush_main_parts()?;
        self.flush_content_types()?;

        Ok(RenderValue::Subdoc(self.build_fragment()))
    }

    // ---------------------------------------------------------------
    // add_referenced_parts / add_relationship：引用部件复制
    // ---------------------------------------------------------------

    /// 上游 `add_referenced_parts`：element 子树内全部带 `r:id` 的元素
    /// （文档序），IMAGE/HEADER/FOOTER 关系跳过（r:id 悬空保留，由
    /// add_images / remove_header_and_footer_references 处理），其余在
    /// 主文档上登记新关系并改写 r:id。
    fn add_referenced_parts(&mut self, element: NodeId) -> Result<(), Error> {
        let mut rid_elements: Vec<(NodeId, String)> = Vec::new();
        {
            let sub_doc = &self.sub_doc;
            for node in sub_doc.descendants(element).into_iter().skip(1) {
                if sub_doc.tag(node).is_some() {
                    if let Some(rid) = sub_doc.attr(node, ns_uri::R, "id") {
                        rid_elements.push((node, rid.to_owned()));
                    }
                }
            }
        }
        for (node, rid) in rid_elements {
            let rel = self.sub_rels.get(&rid).cloned().ok_or_else(|| {
                malformed(format!(
                    "子文档元素引用的关系 {rid:?} 不存在（part {}）",
                    self.sub_main_name
                ))
            })?;
            if rel.rel_type == RT_IMAGE || rel.rel_type == RT_HEADER || rel.rel_type == RT_FOOTER {
                continue;
            }
            let new_rid = self.add_relationship(&rel)?;
            self.sub_doc.set_attr(node, ns_uri::R, "id", new_rid);
        }
        Ok(())
    }

    /// 上游 `add_relationship`：external 关系按（reltype, target）在主
    /// rels 去重复用；internal 关系把目标 part 复制进主包（含递归整图）
    /// 再登记。
    fn add_relationship(&mut self, rel: &Relationship) -> Result<String, Error> {
        if rel.target_mode == TargetMode::External {
            return Ok(self.injections.main_get_or_add(
                &rel.rel_type,
                &rel.target,
                TargetMode::External,
            ));
        }
        let abs_uri = self.resolve_sub_target(&rel.target).ok_or_else(|| {
            malformed(format!(
                "子文档关系目标 {:?} 无法解析为包内 part（part {}）",
                rel.target, self.sub_main_name
            ))
        })?;
        let abs_name = abs_uri.as_str().to_string();
        let part = self.sub_pkg.part(&abs_name).ok_or_else(|| {
            malformed(format!("子文档关系目标 part {abs_name} 不存在（悬空关系）"))
        })?;
        let blob = part.bytes().to_vec();
        let content_type = self
            .sub_pkg
            .content_types()
            .content_type_of(&abs_uri)
            .ok_or_else(|| malformed(format!("子文档 part {abs_name} 缺少 content type 声明")))?
            .to_string();
        let new_name = self.copy_part(abs_uri.as_str(), blob, content_type)?;
        let rel_target = relative_to_owner(&self.main_name, &new_name);
        Ok(self
            .injections
            .main_get_or_add(&rel.rel_type, &rel_target, TargetMode::Internal))
    }

    /// 把 sub 包 part（含其 rels 子图，递归）复制进主包，返回新 part 名。
    ///
    /// 对齐上游 `add_relationship` 的 internal 分支：
    /// - partname 按前缀重新编号：`FILENAME_IDX_RE` 前缀 + 主包同名前缀
    ///   parts 已占编号的回填空洞（`range(1, len+2)` 首个未用）；
    /// - 源 part 的 rels 按 rId 数字排序逐个复制（external 原样登记、
    ///   internal 目标 part 递归复制），副本 rels 的 rId 由 get_or_add
    ///   语义重新分配（源 rId 连续时与源一致）；
    /// - Content Types 按 python-docx `_ContentTypesItem._add_part` 登记。
    fn copy_part(
        &mut self,
        src_abs: &str,
        blob: Vec<u8>,
        content_type: String,
    ) -> Result<String, Error> {
        let prefix = filename_idx_prefix(src_abs).ok_or_else(|| {
            malformed(format!(
                "part 名 {src_abs:?} 不匹配 FILENAME_IDX_RE（非字母开头），无法复制部件"
            ))
        })?;
        let ext = extension_of(src_abs);

        // 主包（含本轮已复制落包的）同名前缀 parts 的占用编号。
        let mut used: HashSet<i64> = HashSet::new();
        for part in self.pkg.parts() {
            let name = part.uri().as_str();
            if name.starts_with(&prefix) {
                if let Some(number) = filename_idx_number(name, prefix.len()) {
                    used.insert(number);
                }
            }
        }
        // 上游 `range(1, len(used)+2)` 首个未用编号。
        let next_number = (1..=(used.len() as i64 + 1))
            .find(|n| !used.contains(n))
            .ok_or_else(|| malformed("部件编号分配失败（不可达）"))?;
        let new_name = format!("{prefix}{next_number}.{ext}");

        // 递归复制源 part 的 rels（rId 数字序）。
        let src_uri = PartUri::new(src_abs)?;
        let mut new_rels = Relationships::default();
        if let Some(rels) = self.sub_pkg.relationships_of(src_abs) {
            let mut sorted: Vec<(u32, Relationship)> = Vec::new();
            for rel in rels.iter() {
                sorted.push((rid_number(&rel.id)?, rel.clone()));
            }
            sorted.sort_by_key(|(number, _)| *number);
            for (_, rel) in sorted {
                match rel.target_mode {
                    TargetMode::External => {
                        get_or_add_rel(
                            &mut new_rels,
                            &rel.rel_type,
                            &rel.target,
                            TargetMode::External,
                        );
                    }
                    TargetMode::Internal => {
                        let base = src_uri.parent();
                        let grand_uri = resolve_part_target(base.as_ref(), &rel.target)
                            .ok_or_else(|| {
                                malformed(format!(
                                    "子文档 part {src_abs} 的关系目标 {:?} 无法解析",
                                    rel.target
                                ))
                            })?;
                        let grand_part =
                            self.sub_pkg.part(grand_uri.as_str()).ok_or_else(|| {
                                malformed(format!(
                                    "子文档关系目标 part {} 不存在（悬空关系）",
                                    grand_uri.as_str()
                                ))
                            })?;
                        let grand_ct = self
                            .sub_pkg
                            .content_types()
                            .content_type_of(&grand_uri)
                            .ok_or_else(|| {
                                malformed(format!(
                                    "子文档 part {} 缺少 content type 声明",
                                    grand_uri.as_str()
                                ))
                            })?
                            .to_string();
                        let grand_new = self.copy_part(
                            grand_uri.as_str(),
                            grand_part.bytes().to_vec(),
                            grand_ct,
                        )?;
                        // 副本 rels 的 Target 相对新 part 目录重算
                        // （python-docx relate_to 传 Part 对象，序列化时
                        //  按双方 partname 计算相对引用）。
                        let rel_target = relative_to_owner(&new_name, &grand_new);
                        get_or_add_rel(
                            &mut new_rels,
                            &rel.rel_type,
                            &rel_target,
                            TargetMode::Internal,
                        );
                    }
                }
            }
        }

        // 落包：part 字节原样复制；Content Types 登记；rels 物化。
        self.pkg.add_part(&new_name, blob)?;
        self.register_content_type(&new_name, &content_type);
        if !new_rels.is_empty() {
            let rels_name = relationships_path_of(&PartUri::new(&new_name)?);
            self.pkg
                .add_part(&rels_name, new_rels.to_xml().into_bytes())?;
            self.register_content_type(&rels_name, CT_RELS);
        }
        Ok(new_name)
    }

    /// python-docx `_ContentTypesItem._add_part`：同扩展名 Default 的
    /// content type 一致则不动；同扩展名异 content type 写 Override；
    /// 扩展名无 Default 则新增 Default。
    ///
    /// （上游还有"Override 已存在则不动"分支：新 part 编号唯一，不会与
    /// 既有 Override 撞名，该分支不可达，从略。）
    fn register_content_type(&mut self, part_name: &str, content_type: &str) {
        let ext = extension_of(part_name);
        let existing_default = self
            .content_types
            .defaults()
            .find(|(known, _)| known.eq_ignore_ascii_case(&ext))
            .map(|(_, ct)| ct.to_string());
        match existing_default {
            Some(ct) if ct == content_type => {}
            Some(_) => {
                self.content_types.add_override(part_name, content_type);
                self.ct_dirty = true;
            }
            None => {
                self.content_types.add_default(&ext, content_type);
                self.ct_dirty = true;
            }
        }
    }

    /// sub 关系目标解析为 sub 包内绝对 part 名。
    fn resolve_sub_target(&self, target: &str) -> Option<PartUri> {
        let base = PartUri::new(&self.sub_main_name).ok()?.parent();
        resolve_part_target(base.as_ref(), target)
    }

    // ---------------------------------------------------------------
    // add_styles：样式三分支合并
    // ---------------------------------------------------------------

    /// body 子级上的 add_styles（element ∈ sub document 树）。
    fn add_styles_in_subdoc(&mut self, element: NodeId) -> Result<(), Error> {
        let sub_doc = &mut self.sub_doc;
        merge_styles(
            sub_doc,
            element,
            &self.sub_styles,
            &mut self.main_styles,
            &self.style_id2name,
            &self.style_name2id,
            self.sub_numbering.as_ref(),
            &mut self.main_numbering,
            &mut self.num_id_mapping,
            &mut self.anum_id_mapping,
            &mut self.main_styles_dirty,
            &mut self.main_numbering_dirty,
        )
    }

    /// 上游 `add_styles_from_other_parts`：sub footnotes part 根元素上的
    /// 样式合并（sub 无 footnotes part 时上游 KeyError → pass）。
    fn add_styles_from_other_parts(&mut self) -> Result<(), Error> {
        let Some(footnotes_tree) = self.sub_footnotes.as_mut() else {
            return Ok(());
        };
        let element = footnotes_tree.root();
        merge_styles(
            footnotes_tree,
            element,
            &self.sub_styles,
            &mut self.main_styles,
            &self.style_id2name,
            &self.style_name2id,
            self.sub_numbering.as_ref(),
            &mut self.main_numbering,
            &mut self.num_id_mapping,
            &mut self.anum_id_mapping,
            &mut self.main_styles_dirty,
            &mut self.main_numbering_dirty,
        )
    }

    // ---------------------------------------------------------------
    // add_numberings：编号复制
    // ---------------------------------------------------------------

    /// body 子级上的 add_numberings（element ∈ sub document 树）。
    fn add_numberings_in_subdoc(&mut self, element: NodeId) -> Result<(), Error> {
        let sub_doc = &mut self.sub_doc;
        merge_numberings(
            sub_doc,
            element,
            self.sub_numbering.as_ref(),
            &mut self.main_numbering,
            &mut self.num_id_mapping,
            &mut self.anum_id_mapping,
            &mut self.main_numbering_dirty,
        )
    }

    // ---------------------------------------------------------------
    // restart_first_numbering：列表编号重启（触发即不支持）
    // ---------------------------------------------------------------

    /// 上游 `restart_first_numbering` 的守卫链：restart_numbering 恒真
    /// （SubdocComposer 未覆盖）。走通守卫链即进入修改块（复制 w:num、
    /// 注入 lvlOverride/startOverride、改写段落 numPr）——该修改依赖
    /// `_next_numbering_ids` 的全包语义且上游测试未覆盖，视为主包
    /// 不支持；bullet 与 heading（outlineLvl）等正常路径不触发。
    fn restart_first_numbering(&mut self, element: NodeId) -> Result<(), Error> {
        let sub_doc = &self.sub_doc;
        let main_styles = &self.main_styles;

        // 无 pStyle → return（正常路径）。
        let Some(style_id) = tag_descendants(sub_doc, element, ns_uri::W, "pStyle")
            .iter()
            .find_map(|node| sub_doc.attr(*node, ns_uri::W, "val").map(str::to_owned))
        else {
            return Ok(());
        };
        // 同一样式已重启 → return。
        if self.numbering_restarted.contains(&style_id) {
            return Ok(());
        }
        // 上游以**主**样式表查该 style_id（注意：未做 name 映射）；
        // 主无此样式 → return。
        let Some(style_el) = style_by_id(main_styles, &style_id) else {
            return Ok(());
        };
        // outlineLvl（标题类）→ 不重启，return。
        if !tag_descendants(main_styles, style_el, ns_uri::W, "outlineLvl").is_empty() {
            return Ok(());
        }

        // 段落直连 numId 优先，其次样式里的 numId；皆无 → return。
        let local_num_id = tag_descendants(sub_doc, element, ns_uri::W, "numId")
            .iter()
            .find_map(|node| sub_doc.attr(*node, ns_uri::W, "val").map(str::to_owned));
        let num_id = match local_num_id {
            Some(value) => value,
            None => match tag_descendants(main_styles, style_el, ns_uri::W, "numId")
                .iter()
                .find_map(|node| main_styles.attr(*node, ns_uri::W, "val").map(str::to_owned))
            {
                Some(value) => value,
                None => return Ok(()),
            },
        };

        // 主 numbering part：上游缺失时从内置模板新建，本实现不支持。
        let Some(main_numbering) = self.main_numbering.as_ref() else {
            return Err(malformed(
                "主文档缺少编号部件（word/numbering.xml），无法重启子文档列表编号",
            ));
        };
        // 主 numbering 无对应 w:num → return（样式无编号元素）。
        let Some(num_el) = num_by_id(main_numbering, &num_id) else {
            return Ok(());
        };
        // w:num 缺 abstractNumId：上游 xpath(...)[0] 直接 IndexError。
        let Some(anum_val) = tag_descendants(main_numbering, num_el, ns_uri::W, "abstractNumId")
            .iter()
            .find_map(|node| {
                main_numbering
                    .attr(*node, ns_uri::W, "val")
                    .map(str::to_owned)
            })
        else {
            return Err(malformed(format!(
                "主编号部件 w:num[@w:numId={num_id}] 缺少 w:abstractNumId（part word/numbering.xml）"
            )));
        };
        // 无对应 w:abstractNum：上游 anum_element[0] 直接 IndexError。
        let Some(anum_el) = anum_by_id(main_numbering, &anum_val) else {
            return Err(malformed(format!(
                "主编号部件缺少 w:abstractNum[@w:abstractNumId={anum_val}]（part word/numbering.xml）"
            )));
        };
        // bullet 不重启 → return；numFmt 缺失时上游继续走修改块。
        let num_fmt = tag_descendants(main_numbering, anum_el, ns_uri::W, "lvl")
            .into_iter()
            .filter(|lvl| main_numbering.attr(*lvl, ns_uri::W, "ilvl") == Some("0"))
            .find_map(|lvl| {
                tag_descendants(main_numbering, lvl, ns_uri::W, "numFmt")
                    .iter()
                    .find_map(|node| {
                        main_numbering
                            .attr(*node, ns_uri::W, "val")
                            .map(str::to_owned)
                    })
            });
        if num_fmt.as_deref() == Some("bullet") {
            return Ok(());
        }

        // 修改块：视为主包不支持（上游依赖随机 nsid 之外的完整重启语义，
        // oracle 语料不覆盖，保守拒绝以免字节漂移）。
        Err(malformed(format!(
            "子文档样式 {style_id:?} 触发列表编号重启（restart_first_numbering），主包不支持"
        )))
    }

    // ---------------------------------------------------------------
    // add_images：图片合并
    // ---------------------------------------------------------------

    /// 上游 `add_images`：`(.//a:blip|.//asvg:svgBlip)[@r:embed]` 文档序，
    /// 按源 part 字节 sha1 复用/新建主包图片 part（`ImageWrapper` 的
    /// 扩展名取源 part 文件名后缀、content type 取源包声明），r:embed
    /// 改写为主 rels 的 rId；`r:link` 外链按 add_relationship 登记。
    fn add_images(&mut self, element: NodeId) -> Result<(), Error> {
        let mut blips = Vec::new();
        {
            let sub_doc = &self.sub_doc;
            for node in sub_doc.descendants(element).into_iter().skip(1) {
                let is_blip = is_tag(sub_doc, node, ns_uri::A, "blip")
                    || is_tag(sub_doc, node, NS_ASVG, "svgBlip");
                if is_blip && sub_doc.attr(node, ns_uri::R, "embed").is_some() {
                    blips.push(node);
                }
            }
        }
        for blip in blips {
            let rid = self
                .sub_doc
                .attr(blip, ns_uri::R, "embed")
                .map(str::to_owned)
                .unwrap_or_default();
            let rel = self.sub_rels.get(&rid).cloned().ok_or_else(|| {
                malformed(format!(
                    "子文档图片引用的关系 {rid:?} 不存在（part {}）",
                    self.sub_main_name
                ))
            })?;
            if rel.target_mode == TargetMode::External {
                return Err(malformed(format!(
                    "子文档图片关系 {rid:?} 为外部引用，不支持跨包外部图片"
                )));
            }
            let abs_uri = self.resolve_sub_target(&rel.target).ok_or_else(|| {
                malformed(format!(
                    "子文档图片关系目标 {:?} 无法解析为包内 part（part {}）",
                    rel.target, self.sub_main_name
                ))
            })?;
            let abs_name = abs_uri.as_str().to_string();
            let part = self.sub_pkg.part(&abs_name).ok_or_else(|| {
                malformed(format!("子文档图片 part {abs_name} 不存在（悬空关系）"))
            })?;
            let content_type = self
                .sub_pkg
                .content_types()
                .content_type_of(&abs_uri)
                .ok_or_else(|| {
                    malformed(format!("子文档图片 part {abs_name} 缺少 content type 声明"))
                })?
                .to_string();
            let ext = extension_of(&abs_name);
            let new_rid = self
                .injections
                .add_subdoc_image(part.bytes(), &ext, &content_type);
            self.sub_doc.set_attr(blip, ns_uri::R, "embed", new_rid);

            // r:link：图片可同时带嵌入与外链（上游经 add_relationship）。
            if let Some(link_rid) = self
                .sub_doc
                .attr(blip, ns_uri::R, "link")
                .map(str::to_owned)
            {
                let rel = self.sub_rels.get(&link_rid).cloned().ok_or_else(|| {
                    malformed(format!(
                        "子文档图片外链关系 {link_rid:?} 不存在（part {}）",
                        self.sub_main_name
                    ))
                })?;
                let new_link = self.add_relationship(&rel)?;
                self.sub_doc.set_attr(blip, ns_uri::R, "link", new_link);
            }
        }
        Ok(())
    }

    // ---------------------------------------------------------------
    // 不支持的引用形态检测（上游 add_diagrams/add_shapes/add_footnotes）
    // ---------------------------------------------------------------

    /// 上游 `add_diagrams`：SmartArt 的关系重定向把 sub 包 part 直挂主
    /// rels（保存时部件不可达），且 oracle 语料不覆盖，主包不支持。
    fn check_diagrams(&mut self, element: NodeId) -> Result<(), Error> {
        let sub_doc = &self.sub_doc;
        let hit = sub_doc
            .descendants(element)
            .into_iter()
            .skip(1)
            .any(|node| {
                is_tag(sub_doc, node, NS_DGM, "relIds")
                    && sub_doc.attr(node, ns_uri::R, "dm").is_some()
            });
        if hit {
            return Err(malformed(
                "子文档含 SmartArt 图表（dgm:relIds），其部件合并主包不支持",
            ));
        }
        Ok(())
    }

    /// 上游 `add_shapes`：VML 形状图片（v:shape/v:imagedata）。
    fn check_shapes(&mut self, element: NodeId) -> Result<(), Error> {
        let sub_doc = &self.sub_doc;
        let hit = sub_doc
            .descendants(element)
            .into_iter()
            .skip(1)
            .any(|node| {
                is_tag(sub_doc, node, ns_uri::V, "shape")
                    && sub_doc
                        .children(node)
                        .iter()
                        .any(|child| is_tag(sub_doc, *child, ns_uri::V, "imagedata"))
            });
        if hit {
            return Err(malformed(
                "子文档含 VML 形状图片（v:shape/v:imagedata），主包不支持",
            ));
        }
        Ok(())
    }

    /// 上游 `add_footnotes`：脚注引用需要把 sub 脚注逐条并入主脚注部件
    /// （改写 id），oracle 语料不覆盖，主包不支持。
    fn check_footnotes(&mut self, element: NodeId) -> Result<(), Error> {
        let sub_doc = &self.sub_doc;
        let hit = !tag_descendants(sub_doc, element, ns_uri::W, "footnoteReference").is_empty();
        if hit {
            return Err(malformed(
                "子文档含脚注引用（w:footnoteReference），脚注合并主包不支持",
            ));
        }
        Ok(())
    }

    /// 上游 `remove_header_and_footer_references`：删除子树内全部
    /// `w:headerReference` / `w:footerReference`（引用留在片段会悬空）。
    fn remove_header_and_footer_references(&mut self, element: NodeId) {
        let sub_doc = &mut self.sub_doc;
        let mut refs = Vec::new();
        for node in sub_doc.descendants(element).into_iter().skip(1) {
            if is_tag(sub_doc, node, ns_uri::W, "headerReference")
                || is_tag(sub_doc, node, ns_uri::W, "footerReference")
            {
                refs.push(node);
            }
        }
        for node in refs {
            sub_doc.detach(node);
        }
    }

    // ---------------------------------------------------------------
    // renumber 三兄弟 + fix_section_types（作用于主文档）
    // ---------------------------------------------------------------

    /// 上游 `renumber_bookmarks`：主 body 的 `w:bookmarkStart` 与
    /// `w:bookmarkEnd` **各自独立**从 0 连续重编（文档序）。
    fn renumber_bookmarks(&mut self) {
        let main_body = match body_of(&self.main_doc) {
            Ok(body) => body,
            Err(_) => return,
        };
        let mut dirty = false;
        for local in ["bookmarkStart", "bookmarkEnd"] {
            for (index, node) in tag_descendants(&self.main_doc, main_body, ns_uri::W, local)
                .into_iter()
                .enumerate()
            {
                if set_attr_if_changed(
                    &mut self.main_doc,
                    node,
                    ns_uri::W,
                    "id",
                    &(index as i64).to_string(),
                ) {
                    dirty = true;
                }
            }
        }
        if dirty {
            self.main_doc_dirty = true;
        }
    }

    /// 上游 `renumber_docpr_ids` / `renumber_nvpicpr_ids` 共用骨架：
    /// 主 body 从 1 连续重编，再按主 rels 的 HEADER/FOOTER 关系（插入序、
    /// 不去重）在各页眉页脚 part 上**续同一计数器**（计数器跨循环延续，
    /// enumerate 不适用）。
    #[allow(clippy::explicit_counter_loop)]
    fn renumber_ids(&mut self, ns: &str, local: &str) -> Result<(), Error> {
        let main_body = body_of(&self.main_doc)?;
        let mut next_id = 1i64;
        let mut body_dirty = false;
        for node in tag_descendants(&self.main_doc, main_body, ns, local) {
            if set_attr_if_changed(&mut self.main_doc, node, "", "id", &next_id.to_string()) {
                body_dirty = true;
            }
            next_id += 1;
        }
        if body_dirty {
            self.main_doc_dirty = true;
        }

        for part_name in self.injections.main_header_footer_parts() {
            let tree = self.hf_tree(&part_name)?;
            let mut dirty = false;
            for node in tag_descendants(tree, tree.root(), ns, local) {
                if set_attr_if_changed(tree, node, "", "id", &next_id.to_string()) {
                    dirty = true;
                }
                next_id += 1;
            }
            if dirty {
                self.dirty_hf.insert(part_name);
            }
        }
        Ok(())
    }

    /// 页眉页脚 part 树（解析一次，多次重编共用；同 part 多关系时上游
    /// 反复 set，此处缓存树保证幂等一致）。
    fn hf_tree(&mut self, name: &str) -> Result<&mut XmlDocument, Error> {
        if !self.hf_trees.contains_key(name) {
            let tree = load_xml_tree(self.pkg, name)?;
            self.hf_trees.insert(name.to_string(), tree);
        }
        match self.hf_trees.get_mut(name) {
            Some(tree) => Ok(tree),
            // 不可达：上方刚插入。
            None => Err(malformed("内部错误：页眉页脚树缓存缺失")),
        }
    }

    /// 上游 `fix_section_types`：任一侧单节即返回；两侧均多节时需要改主
    /// 分节的起始类型（`w:sectPr/w:type`），oracle 语料不覆盖，主包不支持。
    fn fix_section_types(&mut self) -> Result<(), Error> {
        let main_body = body_of(&self.main_doc)?;
        let sub_body = body_of(&self.sub_doc)?;
        let main_sections = section_count(&self.main_doc, main_body);
        let sub_sections = section_count(&self.sub_doc, sub_body);
        if main_sections <= 1 || sub_sections <= 1 {
            return Ok(());
        }
        Err(malformed(format!(
            "主文档（{main_sections} 节）与子文档（{sub_sections} 节）均存在多个分节，\
             fix_section_types 需修改主分节起始类型，主包不支持"
        )))
    }

    // ---------------------------------------------------------------
    // 落盘与片段
    // ---------------------------------------------------------------

    /// 主树写回：仅写实际修改过的 part（python-docx 序列化形态：
    /// 剥空白树 + lxml 单引号声明；模板本身即该形态，未改 part 与
    /// 重序列化字节一致，故 dirty 门控不改变输出、只省写回）。
    fn flush_main_parts(&mut self) -> Result<(), Error> {
        if self.main_doc_dirty {
            let bytes = self.main_doc.serialize().into_bytes();
            self.pkg.set_part_bytes(&self.main_name, bytes)?;
        }
        if self.main_styles_dirty {
            let bytes = self.main_styles.serialize().into_bytes();
            self.pkg.set_part_bytes(&self.main_styles_name, bytes)?;
        }
        if self.main_numbering_dirty {
            let name = self.main_numbering_name.clone().ok_or_else(|| {
                malformed("主文档缺少编号部件（word/numbering.xml），无法写回编号变更")
            })?;
            let bytes = self
                .main_numbering
                .as_ref()
                .ok_or_else(|| malformed("内部错误：编号树缺失"))?
                .serialize()
                .into_bytes();
            self.pkg.set_part_bytes(&name, bytes)?;
        }
        for (name, tree) in &self.hf_trees {
            if self.dirty_hf.contains(name) {
                let bytes = tree.serialize().into_bytes();
                self.pkg.set_part_bytes(name, bytes)?;
            }
        }
        Ok(())
    }

    /// Content Types 变更写回（图片扩展名的 Default 走 ImageInjections
    /// 在 finish 落包，此处只写部件复制产生的条目）。
    fn flush_content_types(&mut self) -> Result<(), Error> {
        if self.ct_dirty {
            let bytes = self.content_types.to_xml().into_bytes();
            self.pkg.set_part_bytes("[Content_Types].xml", bytes)?;
        }
        Ok(())
    }

    /// 上游 `Subdoc._get_xml`：移除 body 直接 `w:sectPr`，其余子级按
    /// 文档序拼接为片段（无 XML 声明、无命名空间声明——上游 tostring
    /// 后正则剥 body 开闭标签，提升到 body 标签上的声明一并丢失）。
    fn build_fragment(&mut self) -> String {
        let Ok(sub_body) = body_of(&self.sub_doc) else {
            return String::new();
        };
        // 移除 body 直接 sectPr（_get_xml 首步）。
        let sectprs: Vec<NodeId> = self
            .sub_doc
            .children(sub_body)
            .iter()
            .copied()
            .filter(|&child| is_tag(&self.sub_doc, child, ns_uri::W, "sectPr"))
            .collect();
        for node in sectprs {
            self.sub_doc.detach(node);
        }
        let mut fragment = String::new();
        for child in self.sub_doc.children(sub_body).to_vec() {
            fragment.push_str(&self.sub_doc.serialize_subtree(child));
        }
        fragment
    }
}

// =====================================================================
// 树级自由函数（同时操作多棵树，参数显式传递以规避借用冲突）
// =====================================================================

/// 上游 `add_styles`（docxcompose composer.py）：
/// - `used_style_ids`：element 子树内 `w:tblStyle|w:pStyle|w:rStyle` 的
///   `w:val`，文档序保序去重；
/// - 每个样式：`mapped_style_id`（sub id → name → 主 id）后走分支：
///   主缺该 id → 深拷贝 sub 样式 append 到主 styles（分支 B），尾随
///   `add_numberings`（副本）与 `add_linked_styles`；主已有 → 不拷贝，
///   仅在 sub 样式带 numId 时建立 anum 映射（分支 C）；
/// - `our_style_id != style_id` 时把 element 子树内 val==style_id 的
///   三类样式引用全部改写为主 id；
/// - `our_style_ids` 每轮循环尾刷新（新拷贝的样式进入主表）。
#[allow(clippy::too_many_arguments)]
fn merge_styles(
    element_tree: &mut XmlDocument,
    element: NodeId,
    sub_styles: &XmlDocument,
    main_styles: &mut XmlDocument,
    style_id2name: &HashMap<String, String>,
    style_name2id: &HashMap<String, String>,
    sub_numbering: Option<&XmlDocument>,
    main_numbering: &mut Option<XmlDocument>,
    num_id_mapping: &mut HashMap<i64, i64>,
    anum_id_mapping: &mut HashMap<i64, i64>,
    main_styles_dirty: &mut bool,
    main_numbering_dirty: &mut bool,
) -> Result<(), Error> {
    let main_styles_root = main_styles.root();
    let mut our_style_ids = style_ids_of(main_styles);

    // 保序去重（OrderedDict.fromkeys）。
    let mut used_style_ids: Vec<String> = Vec::new();
    for node in element_tree.descendants(element).into_iter().skip(1) {
        let is_ref = is_tag(element_tree, node, ns_uri::W, "tblStyle")
            || is_tag(element_tree, node, ns_uri::W, "pStyle")
            || is_tag(element_tree, node, ns_uri::W, "rStyle");
        if is_ref {
            if let Some(val) = element_tree.attr(node, ns_uri::W, "val") {
                let val = val.to_owned();
                if !used_style_ids.contains(&val) {
                    used_style_ids.push(val);
                }
            }
        }
    }

    for style_id in used_style_ids {
        let our_style_id = mapped_style_id(&style_id, style_id2name, style_name2id);
        if !our_style_ids.contains(&our_style_id) {
            // 分支 B：主缺该样式 → 深拷贝 append（get_by_id 无命中时上游
            // deepcopy(None) 为 None，if 守卫跳过）。
            if let Some(src) = style_by_id(sub_styles, &style_id) {
                let copy = main_styles
                    .deepcopy_element(sub_styles, src)
                    .map_err(copy_error)?;
                main_styles.append_child(main_styles_root, copy);
                *main_styles_dirty = true;
                // 尾随 add_numberings(副本) 与 add_linked_styles(副本)。
                merge_numberings(
                    main_styles,
                    copy,
                    sub_numbering,
                    main_numbering,
                    num_id_mapping,
                    anum_id_mapping,
                    main_numbering_dirty,
                )?;
                merge_linked_styles(
                    main_styles,
                    sub_styles,
                    copy,
                    style_id2name,
                    style_name2id,
                    main_styles_dirty,
                )?;
            }
        } else if let Some(sub_style) = style_by_id(sub_styles, &style_id) {
            // 分支 C：主已有该样式 → anum 映射链。各环节缺失即跳过，
            // 嵌套展开（对齐上游 if 链），保证尾段改写始终执行。
            let first_sub_num = tag_descendants(sub_styles, sub_style, ns_uri::W, "numId")
                .iter()
                .find_map(|node| sub_styles.attr(*node, ns_uri::W, "val").map(str::to_owned));
            if let Some(first_sub_num) = first_sub_num {
                // sub numbering 查 numId → abstractNumId（sub 缺编号部件时
                // 上游新建空部件、查询无命中）。
                let sub_anum = sub_numbering.and_then(|tree| {
                    num_by_id(tree, &first_sub_num).and_then(|num_el| {
                        tag_descendants(tree, num_el, ns_uri::W, "abstractNumId")
                            .iter()
                            .find_map(|node| tree.attr(*node, ns_uri::W, "val").map(str::to_owned))
                    })
                });
                if let Some(sub_anum) = sub_anum {
                    // 主样式（our_style_id 在 our_style_ids 中，get_by_id
                    // 必命中）→ 主 numId。
                    if let Some(main_style) = style_by_id(main_styles, &our_style_id) {
                        let first_our_num =
                            tag_descendants(main_styles, main_style, ns_uri::W, "numId")
                                .iter()
                                .find_map(|node| {
                                    main_styles.attr(*node, ns_uri::W, "val").map(str::to_owned)
                                });
                        if let Some(first_our_num) = first_our_num {
                            // 主 numbering：上游缺失时从内置模板新建，
                            // 本实现视为主包不支持。
                            let main_numbering = main_numbering.as_ref().ok_or_else(|| {
                                malformed(
                                    "主文档缺少编号部件（word/numbering.xml），无法建立子文档样式编号映射",
                                )
                            })?;
                            let our_anum =
                                num_by_id(main_numbering, &first_our_num).and_then(|num_el| {
                                    tag_descendants(
                                        main_numbering,
                                        num_el,
                                        ns_uri::W,
                                        "abstractNumId",
                                    )
                                    .iter()
                                    .find_map(|node| {
                                        main_numbering
                                            .attr(*node, ns_uri::W, "val")
                                            .map(str::to_owned)
                                    })
                                });
                            if let Some(our_anum) = our_anum {
                                let sub_key: i64 = sub_anum.parse().map_err(|_| {
                                    malformed(format!("abstractNumId {sub_anum:?} 不是整数"))
                                })?;
                                let main_key: i64 = our_anum.parse().map_err(|_| {
                                    malformed(format!("abstractNumId {our_anum:?} 不是整数"))
                                })?;
                                anum_id_mapping.insert(sub_key, main_key);
                            }
                        }
                    }
                }
            }
        }
        finish_style_refs(element_tree, element, &style_id, &our_style_id);
        // 循环尾刷新主样式表。
        our_style_ids = style_ids_of(main_styles);
    }
    Ok(())
}

/// 样式引用改写（上游 add_styles 循环体尾段）：`our_style_id != style_id`
/// 时把 element 子树内 val==style_id 的 tblStyle/pStyle/rStyle 全部改写。
fn finish_style_refs(
    element_tree: &mut XmlDocument,
    element: NodeId,
    style_id: &str,
    our_style_id: &str,
) {
    if our_style_id == style_id {
        return;
    }
    for node in element_tree.descendants(element).into_iter().skip(1) {
        let is_ref = is_tag(element_tree, node, ns_uri::W, "tblStyle")
            || is_tag(element_tree, node, ns_uri::W, "pStyle")
            || is_tag(element_tree, node, ns_uri::W, "rStyle");
        if is_ref && element_tree.attr(node, ns_uri::W, "val") == Some(style_id) {
            element_tree.set_attr(node, ns_uri::W, "val", our_style_id.to_string());
        }
    }
}

/// 上游 `add_linked_styles`：element（样式副本）的 `w:link/@w:val` 首个
/// 值映射后不在主表时，深拷贝 sub 的链接样式 append 到主 styles。
fn merge_linked_styles(
    main_styles: &mut XmlDocument,
    sub_styles: &XmlDocument,
    element: NodeId,
    style_id2name: &HashMap<String, String>,
    style_name2id: &HashMap<String, String>,
    main_styles_dirty: &mut bool,
) -> Result<(), Error> {
    let Some(linked_id) = tag_descendants(main_styles, element, ns_uri::W, "link")
        .iter()
        .find_map(|node| main_styles.attr(*node, ns_uri::W, "val").map(str::to_owned))
    else {
        return Ok(());
    };
    let our_linked_id = mapped_style_id(&linked_id, style_id2name, style_name2id);
    let our_style_ids = style_ids_of(main_styles);
    if our_style_ids.contains(&our_linked_id) {
        return Ok(());
    }
    // 上游 get_by_id 查 sub 的原 id，命中才 deepcopy append。
    if let Some(src) = style_by_id(sub_styles, &linked_id) {
        let copy = main_styles
            .deepcopy_element(sub_styles, src)
            .map_err(copy_error)?;
        main_styles.append_child(main_styles.root(), copy);
        *main_styles_dirty = true;
    }
    Ok(())
}

/// 上游 `add_numberings`：
/// - num_ids：element 子树内 `w:numId/@w:val` 去重（int 集合，Rust 按
///   升序迭代；CPython 小整数集合的迭代序即升序）；
/// - 空集直接 return（不触主编号部件）；
/// - `_next_numbering_ids` 在循环**前**调用一次：同元素多个 numId 全部
///   映射到同一 next 值（上游行为，照抄）；
/// - 每个 numId：sub 查 `w:num[@w:numId=X]` 无命中 continue（sub 缺
///   编号部件等价空查询）；深拷贝 → numId 改写 → 记 num_id_mapping →
///   副本内首个 `w:abstractNumId`（无则上游 IndexError）→ anum 未映射
///   时 sub 查 `w:abstractNum`：无命中 **continue**（num_id_mapping
///   残留、w:num 不插入、收尾仍按映射改写引用——悬空语义照抄），
///   命中则记 anum 映射、改写副本与 abstractNum 副本、检测 nsid
///   （上游随机化，非确定性 → 不支持）并 `_insert_abstract_num`；
///   已映射则仅改写副本 anum 引用；每轮 `_insert_num`；
/// - 收尾：element 子树内全部 `w:numId` 按映射改写（无映射原样）。
#[allow(clippy::too_many_arguments)]
fn merge_numberings(
    element_tree: &mut XmlDocument,
    element: NodeId,
    sub_numbering: Option<&XmlDocument>,
    main_numbering: &mut Option<XmlDocument>,
    num_id_mapping: &mut HashMap<i64, i64>,
    anum_id_mapping: &mut HashMap<i64, i64>,
    main_numbering_dirty: &mut bool,
) -> Result<(), Error> {
    // num_ids：int 集合（升序迭代）。
    let mut num_ids: Vec<i64> = Vec::new();
    for node in tag_descendants(element_tree, element, ns_uri::W, "numId") {
        if let Some(val) = element_tree.attr(node, ns_uri::W, "val") {
            let value: i64 = val
                .parse()
                .map_err(|_| malformed(format!("w:numId/@w:val {val:?} 不是整数")))?;
            if !num_ids.contains(&value) {
                num_ids.push(value);
            }
        }
    }
    if num_ids.is_empty() {
        return Ok(());
    }
    num_ids.sort_unstable();

    // 主 numbering part：上游缺失时从内置模板新建空部件，本实现不支持。
    let main_numbering = main_numbering
        .as_mut()
        .ok_or_else(|| malformed("主文档缺少编号部件（word/numbering.xml），无法合并子文档编号"))?;
    let (next_num_id, next_anum_id) = next_numbering_ids(main_numbering);

    for num_id in num_ids {
        if num_id_mapping.contains_key(&num_id) {
            continue;
        }
        // sub 查 w:num（sub 缺编号部件 → 空查询 → continue）。
        let Some(sub_tree) = sub_numbering else {
            continue;
        };
        let Some(num_src) = num_by_id_value(sub_tree, num_id) else {
            continue;
        };
        let num_copy = main_numbering
            .deepcopy_element(sub_tree, num_src)
            .map_err(copy_error)?;
        main_numbering.set_attr(num_copy, ns_uri::W, "numId", next_num_id.to_string());
        num_id_mapping.insert(num_id, next_num_id);

        // 副本内首个 w:abstractNumId（上游 //w:abstractNumId 于游离副本
        // 上等价首个后代；缺失直接 IndexError）。
        let anum_node = tag_descendants(main_numbering, num_copy, ns_uri::W, "abstractNumId")
            .into_iter()
            .next();
        let Some(anum_node) = anum_node else {
            return Err(malformed(format!(
                "子文档 w:num[@w:numId={num_id}] 缺少 w:abstractNumId 子元素（part word/numbering.xml）"
            )));
        };
        let anum_val: i64 = main_numbering
            .attr(anum_node, ns_uri::W, "val")
            .ok_or_else(|| malformed("w:abstractNumId 缺少 w:val 属性"))?
            .parse()
            .map_err(|_| malformed("w:abstractNumId/@w:val 不是整数"))?;

        match anum_id_mapping.get(&anum_val) {
            None => {
                // sub 查 w:abstractNum：无命中 continue（映射残留语义照抄）。
                let Some(anum_src) = anum_by_id_value(sub_tree, anum_val) else {
                    continue;
                };
                let anum_copy = main_numbering
                    .deepcopy_element(sub_tree, anum_src)
                    .map_err(copy_error)?;
                anum_id_mapping.insert(anum_val, next_anum_id);
                main_numbering.set_attr(anum_node, ns_uri::W, "val", next_anum_id.to_string());
                main_numbering.set_attr(
                    anum_copy,
                    ns_uri::W,
                    "abstractNumId",
                    next_anum_id.to_string(),
                );
                // nsid 随机化（上游 random）非确定性 → 主包不支持。
                if !tag_descendants(main_numbering, anum_copy, ns_uri::W, "nsid").is_empty() {
                    return Err(malformed(
                        "复制的 w:abstractNum 含 w:nsid：上游按随机数重写 nsid，非确定性输出主包不支持",
                    ));
                }
                insert_abstract_num(main_numbering, anum_copy, main_numbering_dirty);
            }
            Some(mapped) => {
                main_numbering.set_attr(anum_node, ns_uri::W, "val", mapped.to_string());
            }
        }
        insert_num(main_numbering, num_copy, main_numbering_dirty);
    }

    // 收尾：element 子树内全部 w:numId 按映射改写（无映射原样）。
    for node in tag_descendants(element_tree, element, ns_uri::W, "numId") {
        if let Some(val) = element_tree
            .attr(node, ns_uri::W, "val")
            .and_then(|v| v.parse::<i64>().ok())
        {
            if let Some(mapped) = num_id_mapping.get(&val) {
                element_tree.set_attr(node, ns_uri::W, "val", mapped.to_string());
            }
        }
    }
    Ok(())
}

/// 上游 `_next_numbering_ids`：主编号部件 `w:num` 的 numId 最大值 +1
/// （无则 1）；`w:abstractNum` 的 abstractNumId 最大值 +1（无则 0）。
fn next_numbering_ids(tree: &XmlDocument) -> (i64, i64) {
    let root = tree.root();
    let mut next_num_id = 1i64;
    for node in tag_descendants(tree, root, ns_uri::W, "num") {
        if let Some(value) = tree
            .attr(node, ns_uri::W, "numId")
            .and_then(|v| v.parse::<i64>().ok())
        {
            next_num_id = next_num_id.max(value + 1);
        }
    }
    let mut next_anum_id = 0i64;
    for node in tag_descendants(tree, root, ns_uri::W, "abstractNum") {
        if let Some(value) = tree
            .attr(node, ns_uri::W, "abstractNumId")
            .and_then(|v| v.parse::<i64>().ok())
        {
            next_anum_id = next_anum_id.max(value + 1);
        }
    }
    (next_num_id, next_anum_id)
}

/// 上游 `_insert_num`：有 `w:num` 时插在**最后一个 num 之前**（上游注释
/// "after" 与代码相反，按代码对齐）；无 num 时 append 到根。
fn insert_num(tree: &mut XmlDocument, element: NodeId, dirty: &mut bool) {
    let root = tree.root();
    let nums = tag_descendants(tree, root, ns_uri::W, "num");
    if let Some(last) = nums.last().copied() {
        if let Some(parent) = tree.parent(last) {
            if let Some(position) = tree.children(parent).iter().position(|&c| c == last) {
                tree.insert_child_at(parent, position, element);
                *dirty = true;
                return;
            }
        }
    }
    tree.append_child(root, element);
    *dirty = true;
}

/// 上游 `_insert_abstract_num`：有 `w:num` 时插在**第一个 num 之前**；
/// 无 num 时插在根首位（`insert(0)`）。
fn insert_abstract_num(tree: &mut XmlDocument, element: NodeId, dirty: &mut bool) {
    let root = tree.root();
    let nums = tag_descendants(tree, root, ns_uri::W, "num");
    if let Some(first) = nums.first().copied() {
        if let Some(parent) = tree.parent(first) {
            if let Some(position) = tree.children(parent).iter().position(|&c| c == first) {
                tree.insert_child_at(parent, position, element);
                *dirty = true;
                return;
            }
        }
    }
    tree.insert_child_at(root, 0, element);
    *dirty = true;
}

// =====================================================================
// 通用小工具
// =====================================================================

/// 构造主包不支持的 DEV 错误（ADR-007 兼容边界，带上下文说明）。
fn malformed(reason: impl Into<String>) -> Error {
    Error::Opc(OpcError::Malformed {
        reason: reason.into(),
    })
}

/// 跨文档子树拷贝失败：统一归并为主包不支持错误（命名空间前缀冲突等：
/// 上游 lxml deepcopy + append 可自动改名规避，本实现保留词法前缀，
/// 前缀冲突时无法适配）。
fn copy_error(source: docxtpl_xml::XmlError) -> Error {
    malformed(format!("拷贝子文档部件树失败：{source}"))
}

/// 判定节点是否为指定命名空间限定名。
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

/// 文档根的直接 `w:body`（标准 docx 恒存在）。
fn body_of(doc: &XmlDocument) -> Result<NodeId, Error> {
    doc.children(doc.root())
        .iter()
        .copied()
        .find(|&child| is_tag(doc, child, ns_uri::W, "body"))
        .ok_or_else(|| malformed("文档缺少 w:body（part word/document.xml）"))
}

/// 设置属性并在值变化时返回 true（dirty 判定）。
fn set_attr_if_changed(
    doc: &mut XmlDocument,
    id: NodeId,
    ns: &str,
    local: &str,
    value: &str,
) -> bool {
    if doc.attr(id, ns, local) == Some(value) {
        return false;
    }
    doc.set_attr(id, ns, local, value.to_string());
    true
}

/// styles 根的直接子 `w:style[@w:styleId=X]`（python-docx get_by_id）。
fn style_by_id(doc: &XmlDocument, style_id: &str) -> Option<NodeId> {
    doc.children(doc.root()).iter().copied().find(|&child| {
        is_tag(doc, child, ns_uri::W, "style")
            && doc.attr(child, ns_uri::W, "styleId") == Some(style_id)
    })
}

/// styles 根全部 `w:style` 的 styleId（文档序）。
fn style_ids_of(doc: &XmlDocument) -> Vec<String> {
    doc.children(doc.root())
        .iter()
        .filter(|&&child| is_tag(doc, child, ns_uri::W, "style"))
        .filter_map(|&child| doc.attr(child, ns_uri::W, "styleId").map(str::to_owned))
        .collect()
}

/// 样式的 `w:name/@w:val`（直接子 `w:name`；缺失返回 None）。
fn style_name_of(doc: &XmlDocument, style_el: NodeId) -> Option<String> {
    doc.children(style_el)
        .iter()
        .copied()
        .find(|&child| is_tag(doc, child, ns_uri::W, "name"))
        .and_then(|name| doc.attr(name, ns_uri::W, "val").map(str::to_owned))
}

/// sub 样式表 id → name（无 w:name 的样式不建映射，mapped 时原样返回，
/// 与上游 `None` 查 get 默认值的行为一致）。
fn style_id_name_map(doc: &XmlDocument) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for child in style_elements(doc) {
        if let (Some(id), Some(name)) = (
            doc.attr(child, ns_uri::W, "styleId").map(str::to_owned),
            style_name_of(doc, child),
        ) {
            map.insert(id, name);
        }
    }
    map
}

/// 主样式表 name → id。
fn style_name_id_map(doc: &XmlDocument) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for child in style_elements(doc) {
        if let (Some(id), Some(name)) = (
            doc.attr(child, ns_uri::W, "styleId").map(str::to_owned),
            style_name_of(doc, child),
        ) {
            map.insert(name, id);
        }
    }
    map
}

/// styles 根的直接子 `w:style` 节点集合。
fn style_elements(doc: &XmlDocument) -> Vec<NodeId> {
    doc.children(doc.root())
        .iter()
        .copied()
        .filter(|&child| is_tag(doc, child, ns_uri::W, "style"))
        .collect()
}

/// 上游 `mapped_style_id`：sub id → name → 主 id，缺任一环节原样返回。
fn mapped_style_id(
    style_id: &str,
    style_id2name: &HashMap<String, String>,
    style_name2id: &HashMap<String, String>,
) -> String {
    if let Some(name) = style_id2name.get(style_id) {
        if let Some(our_id) = style_name2id.get(name) {
            return our_id.clone();
        }
    }
    style_id.to_owned()
}

/// 按字符串 numId 查 `w:num`（上游 `'.//w:num[@w:numId="%s"]'` 首个命中）。
fn num_by_id(tree: &XmlDocument, num_id: &str) -> Option<NodeId> {
    tag_descendants(tree, tree.root(), ns_uri::W, "num")
        .into_iter()
        .find(|&node| tree.attr(node, ns_uri::W, "numId") == Some(num_id))
}

/// 按整型 numId 查 `w:num`。
fn num_by_id_value(tree: &XmlDocument, num_id: i64) -> Option<NodeId> {
    num_by_id(tree, &num_id.to_string())
}

/// 按字符串 abstractNumId 查 `w:abstractNum`。
fn anum_by_id(tree: &XmlDocument, anum_id: &str) -> Option<NodeId> {
    tag_descendants(tree, tree.root(), ns_uri::W, "abstractNum")
        .into_iter()
        .find(|&node| tree.attr(node, ns_uri::W, "abstractNumId") == Some(anum_id))
}

/// 按整型 abstractNumId 查 `w:abstractNum`。
fn anum_by_id_value(tree: &XmlDocument, anum_id: i64) -> Option<NodeId> {
    anum_by_id(tree, &anum_id.to_string())
}

/// 分节数：段内 `w:pPr/w:sectPr` + body 直接 `w:sectPr`（python-docx
/// Sections 语义）。
fn section_count(doc: &XmlDocument, body: NodeId) -> usize {
    let mut count = 0usize;
    for child in doc.children(body) {
        if is_tag(doc, *child, ns_uri::W, "p") {
            for grand in doc.children(*child) {
                if is_tag(doc, *grand, ns_uri::W, "pPr") {
                    count += doc
                        .children(*grand)
                        .iter()
                        .filter(|&&sect| is_tag(doc, sect, ns_uri::W, "sectPr"))
                        .count();
                }
            }
        } else if is_tag(doc, *child, ns_uri::W, "sectPr") {
            count += 1;
        }
    }
    count
}

/// 读取 part 字节并按 UTF-8 解码。
fn read_part_utf8(pkg: &Package, name: &str) -> Result<String, Error> {
    let bytes = pkg
        .part(name)
        .ok_or_else(|| {
            Error::Opc(OpcError::MissingPart {
                uri: name.to_string(),
            })
        })?
        .bytes()
        .to_vec();
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(source) => {
            let source = source.utf8_error();
            Err(Error::NotUtf8 {
                part: name.to_string(),
                source,
            })
        }
    }
}

/// 解析 part 为树并剥空白（python-docx oxml 解析器 `remove_blank_text`）。
fn load_xml_tree(pkg: &Package, name: &str) -> Result<XmlDocument, Error> {
    let xml = read_part_utf8(pkg, name)?;
    let mut doc = XmlDocument::parse_strict(&xml, &XmlLimits::default()).map_err(|source| {
        Error::Render(docxtpl_template::RenderError::Xml {
            part: name.to_string(),
            source,
        })
    })?;
    doc.strip_blank_text();
    Ok(doc)
}

/// part 主文档 rels 中指定 reltype 的内部目标 part 名
/// （python-docx `part_related_by`；无匹配返回 None）。
fn related_part(pkg: &Package, owner: &str, rel_type: &str) -> Option<String> {
    let owner_uri = PartUri::new(owner).ok()?;
    let base = owner_uri.parent();
    let rels = pkg.relationships_of(owner)?;
    let rel = rels
        .iter()
        .find(|rel| rel.rel_type == rel_type && rel.target_mode == TargetMode::Internal)?;
    resolve_part_target(base.as_ref(), &rel.target).map(|uri| uri.as_str().to_string())
}

/// `FILENAME_IDX_RE = ([a-zA-Z/_-]+)([1-9][0-9]*)?` 的 group(1)：
/// part 名开头的字母/斜杠段；空段（数字等开头）时上游 match 为 None、
/// 取 group 崩溃，返回 None。
fn filename_idx_prefix(name: &str) -> Option<String> {
    let bytes = name.as_bytes();
    let mut end = 0usize;
    while end < bytes.len()
        && (bytes[end].is_ascii_alphabetic() || matches!(bytes[end], b'/' | b'_' | b'-'))
    {
        end += 1;
    }
    (end > 0).then(|| name[..end].to_string())
}

/// `FILENAME_IDX_RE` 的 group(2)：前缀段之后紧随的 `[1-9][0-9]*` 数字。
fn filename_idx_number(name: &str, prefix_len: usize) -> Option<i64> {
    let rest = name.get(prefix_len..)?;
    let bytes = rest.as_bytes();
    if bytes.is_empty() || bytes[0] == b'0' || !bytes[0].is_ascii_digit() {
        return None;
    }
    let end = bytes
        .iter()
        .position(|byte| !byte.is_ascii_digit())
        .unwrap_or(bytes.len());
    rest[..end].parse().ok()
}

/// part 名的文件扩展名（python-docx `PackURI.ext`：文件名含点则取末段，
/// 否则空串）。
fn extension_of(name: &str) -> String {
    let file_name = name.rsplit('/').next().unwrap_or(name);
    match file_name.rsplit_once('.') {
        Some((_, ext)) => ext.to_string(),
        None => String::new(),
    }
}

/// `RID_IDX_RE = rId([0-9]*)`：关系 Id 的数字后缀（空后缀上游 int("") 崩）。
fn rid_number(rid: &str) -> Result<u32, Error> {
    let digits = rid
        .strip_prefix("rId")
        .ok_or_else(|| malformed(format!("关系 Id {rid:?} 不匹配 rIdN 形式")))?;
    digits
        .parse::<u32>()
        .map_err(|_| malformed(format!("关系 Id {rid:?} 缺少数字后缀")))
}

/// `rels.get_or_add` 语义（python-docx）：（reltype, target, mode）全等
/// 复用既有 rId，否则按 rId1 起回填空洞新增。
fn get_or_add_rel(
    rels: &mut Relationships,
    rel_type: &str,
    target: &str,
    mode: TargetMode,
) -> String {
    if let Some(rel) = rels.find_matching(rel_type, target, mode) {
        return rel.id.clone();
    }
    let id = rels.next_r_id();
    rels.push(Relationship {
        id: id.clone(),
        rel_type: rel_type.to_string(),
        target: target.to_string(),
        target_mode: mode,
    });
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filename_idx_matches_upstream_regex() {
        assert_eq!(
            filename_idx_prefix("word/footer1.xml").as_deref(),
            Some("word/footer")
        );
        assert_eq!(
            filename_idx_prefix("word/media/image1.png").as_deref(),
            Some("word/media/image")
        );
        assert_eq!(
            filename_idx_prefix("word/header.xml").as_deref(),
            Some("word/header")
        );
        // 上游 partname 带前导 /，group(1) 含 /
        assert_eq!(
            filename_idx_prefix("/word/footer1.xml").as_deref(),
            Some("/word/footer")
        );
        // 数字开头：上游 match None → AttributeError
        assert_eq!(filename_idx_prefix("1word/a.xml"), None);
        // group(2)
        assert_eq!(
            filename_idx_number("word/footer2.xml", "word/footer".len()),
            Some(2)
        );
        assert_eq!(
            filename_idx_number("word/footer.xml", "word/footer".len()),
            None
        );
        assert_eq!(
            filename_idx_number("word/footer01.xml", "word/footer".len()),
            None
        );
        assert_eq!(
            filename_idx_number("word/footerX1.xml", "word/footer".len()),
            None
        );
        assert_eq!(
            filename_idx_number("word/footer3x.png", "word/footer".len()),
            Some(3)
        );
    }

    #[test]
    fn extension_matches_packuri_ext() {
        assert_eq!(extension_of("word/media/image1.png"), "png");
        assert_eq!(extension_of("_rels/.rels"), "rels");
        assert_eq!(extension_of("word/noext"), "");
    }

    #[test]
    fn rid_number_parses() {
        assert_eq!(rid_number("rId1").ok(), Some(1));
        assert_eq!(rid_number("rId12").ok(), Some(12));
        assert!(rid_number("rId").is_err());
        assert!(rid_number("Id1").is_err());
    }
}

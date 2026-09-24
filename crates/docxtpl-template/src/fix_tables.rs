//! 上游 `fix_tables` 与 `fix_docpr_ids` 的移植（template.py L519–614）。
//!
//! 命名空间一律按 URI 匹配（等价 lxml `ns + "tbl"` 用法，见代码规范 §4.2）。
//! 算法逐行对齐上游；上游会抛异常的输入（缺 tblGrid、gridCol 无 w 宽度、
//! gridSpan 无 val）在本侧返回 [`RenderError`] 而非 panic——corpus 不触发。

use docxtpl_xml::ns_uri;
use docxtpl_xml::{NodeId, XmlDocument, XmlError};

use crate::error::{RenderError, TemplateErrorKind};

/// 节点是否为命名空间 `w` 下的 `local` 元素。
fn is_w(doc: &XmlDocument, id: NodeId, local: &str) -> bool {
    doc.tag(id)
        .is_some_and(|q| q.ns == ns_uri::W && q.local == local)
}

/// 取直接子元素中的全部 `w:local`（等价 lxml `element.findall(ns+local)`，
/// 只计直接子节点，不进嵌套表）。
fn direct_w_children(doc: &XmlDocument, id: NodeId, local: &str) -> Vec<NodeId> {
    doc.children(id)
        .iter()
        .copied()
        .filter(|&c| is_w(doc, c, local))
        .collect()
}

/// 构造“结构不满足上游假设”的模板错误（对应上游在该输入下抛 Python 异常）。
fn malformed(message: impl Into<String>) -> RenderError {
    RenderError::Template {
        kind: TemplateErrorKind::Other,
        part: "word/document.xml".to_string(),
        line: None,
        message: message.into(),
        context: Vec::new(),
    }
}

/// 把 XML 错误包上 part 名。
fn wrap_xml(err: XmlError) -> RenderError {
    RenderError::Xml {
        part: "word/document.xml".to_string(),
        source: err,
    }
}

/// 对齐 `DocxTemplate.fix_tables`：
/// tc 循环可能使单元格数与 tblGrid 列数不一致，本函数增删 gridCol 并按比例调整宽度。
///
/// 注意：与上游一致，行的枚举用“全部后代 `w:tr`”（含嵌套表中的行），
/// 而每行的单元格只取直接子 `w:tc`。
pub fn fix_tables(doc: &mut XmlDocument) -> Result<(), RenderError> {
    let root = doc.root();
    let tables: Vec<NodeId> = doc
        .descendants(root)
        .into_iter()
        .filter(|&n| is_w(doc, n, "tbl"))
        .collect();

    for table in tables {
        // tblGrid 是直接子元素
        let tbl_grid = *doc
            .children(table)
            .iter()
            .find(|&&c| is_w(doc, c, "tblGrid"))
            .ok_or_else(|| malformed("fix_tables: w:tbl 缺少直接子节点 w:tblGrid"))?;

        let mut columns = direct_w_children(doc, tbl_grid, "gridCol");
        let rows = doc
            .descendants(table)
            .into_iter()
            .filter(|&n| is_w(doc, n, "tr"))
            .collect::<Vec<_>>();

        // ---- 加列：行内直接 tc 数超过 gridCol 数 ----
        let mut to_add = 0usize;
        for row in &rows {
            let cell_count = direct_w_children(doc, *row, "tc").len();
            if columns.len() + to_add < cell_count {
                to_add = cell_count - columns.len();
            }
        }

        if to_add > 0 {
            let mut width = 0.0f64;
            for c in &columns {
                if let Some(w) = doc.attr(*c, ns_uri::W, "w") {
                    width += w.parse::<f64>().map_err(|_| {
                        malformed(format!("fix_tables: gridCol w:w={w:?} 不是数字"))
                    })?;
                }
            }
            if width <= 0.0 {
                // 上游此路径 int(None) 会抛 TypeError：显式报错，不静默产出零宽列。
                return Err(malformed(
                    "fix_tables: 需要新增列但 tblGrid 现有列宽之和为 0（上游在此输入下崩溃）",
                ));
            }
            let old_average = width / columns.len() as f64;
            let new_average = width / (columns.len() + to_add) as f64;
            for c in &columns {
                let old: f64 = doc
                    .attr(*c, ns_uri::W, "w")
                    .ok_or_else(|| malformed("fix_tables: 缩放列缺少 w:w"))?
                    .parse()
                    .map_err(|_| malformed("fix_tables: gridCol w:w 不是数字"))?;
                let scaled = (old * new_average / old_average) as i64;
                doc.set_attr(*c, ns_uri::W, "w", scaled.to_string());
            }
            for _ in 0..to_add {
                let grid_col = doc
                    .new_w_element(
                        "gridCol",
                        vec![("w".to_string(), (new_average as i64).to_string())],
                    )
                    .map_err(wrap_xml)?;
                doc.append_child(tbl_grid, grid_col);
                columns.push(grid_col);
            }
        }

        // ---- 删列：按 gridSpan 折算后的最大单元格宽度小于 gridCol 数 ----
        let columns_len = columns.len();
        let mut cells_len_max = 0usize;
        for row in &rows {
            let mut total = 0usize;
            for cell in direct_w_children(doc, *row, "tc") {
                let mut span = 1i64;
                if let Some(tc_pr) = doc
                    .children(cell)
                    .iter()
                    .copied()
                    .find(|&c| is_w(doc, c, "tcPr"))
                {
                    if let Some(grid_span) = doc
                        .children(tc_pr)
                        .iter()
                        .copied()
                        .find(|&c| is_w(doc, c, "gridSpan"))
                    {
                        span = doc
                            .attr(grid_span, ns_uri::W, "val")
                            .ok_or_else(|| malformed("fix_tables: gridSpan 缺少 w:val"))?
                            .parse()
                            .map_err(|_| malformed("fix_tables: gridSpan w:val 不是整数"))?;
                    }
                }
                total += span.max(0) as usize;
            }
            cells_len_max = cells_len_max.max(total);
        }

        let to_remove = columns_len.saturating_sub(cells_len_max);
        if to_remove > 0 {
            // 重新取一次当前列（与上游 Refetch 后切片 columns[-to_remove:] 一致）
            let current = direct_w_children(doc, tbl_grid, "gridCol");
            let remove_set = current[current.len() - to_remove..].to_vec();
            let mut removed_width = 0.0f64;
            for c in &remove_set {
                let w: f64 = doc
                    .attr(*c, ns_uri::W, "w")
                    .ok_or_else(|| malformed("fix_tables: 待删列缺少 w:w"))?
                    .parse()
                    .map_err(|_| malformed("fix_tables: gridCol w:w 不是数字"))?;
                removed_width += w;
                doc.detach(*c);
            }
            let left = direct_w_children(doc, tbl_grid, "gridCol");
            if !left.is_empty() {
                let extra_space = (removed_width / left.len() as f64) as i64;
                for c in &left {
                    let old: f64 = doc
                        .attr(*c, ns_uri::W, "w")
                        .ok_or_else(|| malformed("fix_tables: 保留列缺少 w:w"))?
                        .parse()
                        .map_err(|_| malformed("fix_tables: gridCol w:w 不是数字"))?;
                    doc.set_attr(*c, ns_uri::W, "w", ((old as i64) + extra_space).to_string());
                }
            }
        }
    }
    Ok(())
}

/// 对齐 `DocxTemplate.fix_docpr_ids`：文档内全部 `wp:docPr` 的 `id`
/// 从 1001 起按文档顺序重新编号（上游 `docx_ids_index` 初值 1000，先自增后赋值）。
pub fn fix_docpr_ids(doc: &mut XmlDocument) {
    let root = doc.root();
    let mut index = 1000i64;
    for node in doc.descendants(root) {
        let is_docpr = doc
            .tag(node)
            .is_some_and(|q| q.ns == ns_uri::WP && q.local == "docPr");
        if is_docpr {
            index += 1;
            // 上游 elt.attrib["id"] 是无前缀属性（属性无默认命名空间），
            // 不可按 wp 命名空间写入，否则会序列化成 wp:id。
            doc.set_attr(node, "", "id", index.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxtpl_xml::XmlLimits;

    const W: &str = docxtpl_xml::ns_uri::W;

    fn parse(xml: &str) -> XmlDocument {
        XmlDocument::parse_strict(xml, &XmlLimits::default()).expect("测试 XML 应良构")
    }

    fn grid_widths(doc: &XmlDocument) -> Vec<String> {
        let grid = doc
            .descendants(doc.root())
            .into_iter()
            .find(|&n| is_w(doc, n, "tblGrid"))
            .expect("应有 tblGrid");
        direct_w_children(doc, grid, "gridCol")
            .into_iter()
            .map(|c| doc.attr(c, W, "w").expect("gridCol 应有 w:w").to_string())
            .collect()
    }

    #[test]
    fn adds_grid_columns_and_scales_widths() {
        // 2 列（各 1000）但行内 3 个 tc：补 1 列，旧列按比例缩放为 666。
        let xml = format!(
            r#"<w:document xmlns:w="{W}">
                 <w:tbl>
                   <w:tblGrid>
                     <w:gridCol w:w="1000"/><w:gridCol w:w="1000"/>
                   </w:tblGrid>
                   <w:tr><w:tc/><w:tc/><w:tc/></w:tr>
                 </w:tbl>
               </w:document>"#
        );
        let mut doc = parse(&xml);
        fix_tables(&mut doc).unwrap();
        assert_eq!(grid_widths(&doc), vec!["666", "666", "666"]);
    }

    #[test]
    fn removes_extra_columns_and_redistributes_width() {
        // 3 列（各 1000）但行内只有 1 个 tc：删 2 列，宽度并入保留列。
        let xml = format!(
            r#"<w:document xmlns:w="{W}">
                 <w:tbl>
                   <w:tblGrid>
                     <w:gridCol w:w="1000"/><w:gridCol w:w="1000"/><w:gridCol w:w="1000"/>
                   </w:tblGrid>
                   <w:tr><w:tc/></w:tr>
                 </w:tbl>
               </w:document>"#
        );
        let mut doc = parse(&xml);
        fix_tables(&mut doc).unwrap();
        assert_eq!(grid_widths(&doc), vec!["3000"]);
    }

    #[test]
    fn missing_tbl_grid_is_error() {
        let xml = format!(r#"<w:document xmlns:w="{W}"><w:tbl><w:tr/></w:tbl></w:document>"#);
        let mut doc = parse(&xml);
        assert!(fix_tables(&mut doc).is_err());
    }

    #[test]
    fn docpr_ids_renumbered_without_prefix() {
        let xml = format!(
            r#"<w:document xmlns:w="{W}" xmlns:wp="{WP}">
                       <wp:docPr id="5" name="A"/>
                       <w:p><wp:docPr id="9" name="B"/></w:p>
                     </w:document>"#,
            W = W,
            WP = docxtpl_xml::ns_uri::WP
        );
        let mut doc = parse(&xml);
        fix_docpr_ids(&mut doc);
        let docprs: Vec<_> = doc
            .descendants(doc.root())
            .into_iter()
            .filter(|&n| {
                doc.tag(n)
                    .is_some_and(|q| q.ns == docxtpl_xml::ns_uri::WP && q.local == "docPr")
            })
            .collect();
        assert_eq!(doc.attr(docprs[0], "", "id"), Some("1001"));
        assert_eq!(doc.attr(docprs[1], "", "id"), Some("1002"));
        // 绝不能产出 wp:id（错误命名空间）。
        let xml_out = doc.serialize();
        assert!(!xml_out.contains("wp:id"));
        assert!(xml_out.contains("id=\"1001\""));
    }
}

//! Port of upstream `fix_tables` and `fix_docpr_ids` (template.py L519–614).
//!
//! Namespaces are always matched by URI (equivalent to the lxml
//! `ns + "tbl"` usage; see code spec §4.2). The algorithm follows upstream
//! line by line; inputs on which upstream raises (missing tblGrid, gridCol
//! without a w width, gridSpan without val) return a [`RenderError`] here
//! instead of panicking — the corpus never triggers them.

use docxtpl_xml::ns_uri;
use docxtpl_xml::{NodeId, XmlDocument, XmlError};

use crate::error::{RenderError, TemplateErrorKind};

/// Whether the node is a `local` element in the `w` namespace.
fn is_w(doc: &XmlDocument, id: NodeId, local: &str) -> bool {
    doc.tag(id)
        .is_some_and(|q| q.ns == ns_uri::W && q.local == local)
}

/// Returns all `w:local` direct child elements (equivalent to lxml
/// `element.findall(ns+local)`; only direct children are considered, without
/// descending into nested tables).
fn direct_w_children(doc: &XmlDocument, id: NodeId, local: &str) -> Vec<NodeId> {
    doc.children(id)
        .iter()
        .copied()
        .filter(|&c| is_w(doc, c, local))
        .collect()
}

/// Builds a template error for "structure does not satisfy upstream
/// assumptions" (corresponds to upstream raising a Python exception on such
/// input).
fn malformed(message: impl Into<String>) -> RenderError {
    RenderError::Template {
        kind: TemplateErrorKind::Other,
        part: "word/document.xml".to_string(),
        line: None,
        message: message.into(),
        context: Vec::new(),
    }
}

/// Wraps an XML error with the part name.
fn wrap_xml(err: XmlError) -> RenderError {
    RenderError::Xml {
        part: "word/document.xml".to_string(),
        source: err,
    }
}

/// Aligned with `DocxTemplate.fix_tables`:
/// tc loops can make the cell count inconsistent with the number of tblGrid
/// columns; this function adds/removes gridCol elements and adjusts widths
/// proportionally.
///
/// Note: as upstream, rows are enumerated via "all descendant `w:tr`"
/// (including rows in nested tables), while each row's cells are taken only
/// from direct `w:tc` children.
pub fn fix_tables(doc: &mut XmlDocument) -> Result<(), RenderError> {
    let root = doc.root();
    let tables: Vec<NodeId> = doc
        .descendants(root)
        .into_iter()
        .filter(|&n| is_w(doc, n, "tbl"))
        .collect();

    for table in tables {
        // tblGrid is a direct child element
        let tbl_grid = *doc
            .children(table)
            .iter()
            .find(|&&c| is_w(doc, c, "tblGrid"))
            .ok_or_else(|| malformed("fix_tables: w:tbl is missing direct child w:tblGrid"))?;

        let mut columns = direct_w_children(doc, tbl_grid, "gridCol");
        let rows = doc
            .descendants(table)
            .into_iter()
            .filter(|&n| is_w(doc, n, "tr"))
            .collect::<Vec<_>>();

        // ---- Add columns: direct tc count in a row exceeds the gridCol count ----
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
                        malformed(format!("fix_tables: gridCol w:w={w:?} is not a number"))
                    })?;
                }
            }
            if width <= 0.0 {
                // Upstream raises TypeError via int(None) on this path: fail
                // explicitly instead of silently producing zero-width columns.
                return Err(malformed(
                    "fix_tables: columns must be added but the sum of the existing tblGrid column widths is 0 (upstream crashes on this input)",
                ));
            }
            let old_average = width / columns.len() as f64;
            let new_average = width / (columns.len() + to_add) as f64;
            for c in &columns {
                let old: f64 = doc
                    .attr(*c, ns_uri::W, "w")
                    .ok_or_else(|| malformed("fix_tables: scaled column is missing w:w"))?
                    .parse()
                    .map_err(|_| malformed("fix_tables: gridCol w:w is not a number"))?;
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

        // ---- Remove columns: the maximum cell width after gridSpan conversion is below the gridCol count ----
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
                            .ok_or_else(|| malformed("fix_tables: gridSpan is missing w:val"))?
                            .parse()
                            .map_err(|_| {
                                malformed("fix_tables: gridSpan w:val is not an integer")
                            })?;
                    }
                }
                total = total.saturating_add(span.max(0) as usize);
            }
            cells_len_max = cells_len_max.max(total);
        }

        let to_remove = columns_len.saturating_sub(cells_len_max);
        if to_remove > 0 {
            // Fetch the current columns again (matches the upstream
            // columns[-to_remove:] slice after Refetch)
            let current = direct_w_children(doc, tbl_grid, "gridCol");
            let remove_set = current[current.len() - to_remove..].to_vec();
            let mut removed_width = 0.0f64;
            for c in &remove_set {
                let w: f64 = doc
                    .attr(*c, ns_uri::W, "w")
                    .ok_or_else(|| malformed("fix_tables: column to remove is missing w:w"))?
                    .parse()
                    .map_err(|_| malformed("fix_tables: gridCol w:w is not a number"))?;
                removed_width += w;
                doc.detach(*c);
            }
            let left = direct_w_children(doc, tbl_grid, "gridCol");
            if !left.is_empty() {
                let extra_space = (removed_width / left.len() as f64) as i64;
                for c in &left {
                    let old: f64 = doc
                        .attr(*c, ns_uri::W, "w")
                        .ok_or_else(|| malformed("fix_tables: retained column is missing w:w"))?
                        .parse()
                        .map_err(|_| malformed("fix_tables: gridCol w:w is not a number"))?;
                    doc.set_attr(*c, ns_uri::W, "w", ((old as i64) + extra_space).to_string());
                }
            }
        }
    }
    Ok(())
}

/// Aligned with `DocxTemplate.fix_docpr_ids`: renumbers the `id` of every
/// `wp:docPr` in the document starting at 1001 in document order (upstream
/// `docx_ids_index` starts at 1000 and is incremented before assignment).
pub fn fix_docpr_ids(doc: &mut XmlDocument) {
    let root = doc.root();
    let mut index = 1000i64;
    for node in doc.descendants(root) {
        let is_docpr = doc
            .tag(node)
            .is_some_and(|q| q.ns == ns_uri::WP && q.local == "docPr");
        if is_docpr {
            index += 1;
            // Upstream elt.attrib["id"] is a prefix-less attribute
            // (attributes have no default namespace); it must not be written
            // in the wp namespace, or it would serialize as wp:id.
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
        XmlDocument::parse_strict(xml, &XmlLimits::default())
            .expect("test XML should be well-formed")
    }

    fn grid_widths(doc: &XmlDocument) -> Vec<String> {
        let grid = doc
            .descendants(doc.root())
            .into_iter()
            .find(|&n| is_w(doc, n, "tblGrid"))
            .expect("there should be a tblGrid");
        direct_w_children(doc, grid, "gridCol")
            .into_iter()
            .map(|c| {
                doc.attr(c, W, "w")
                    .expect("gridCol should have w:w")
                    .to_string()
            })
            .collect()
    }

    #[test]
    fn adds_grid_columns_and_scales_widths() {
        // 2 columns (1000 each) but 3 tcs in the row: add 1 column and scale
        // the old columns proportionally to 666.
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
        // 3 columns (1000 each) but only 1 tc in the row: remove 2 columns
        // and merge their width into the retained column.
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
        // Must never produce wp:id (wrong namespace).
        let xml_out = doc.serialize();
        assert!(!xml_out.contains("wp:id"));
        assert!(xml_out.contains("id=\"1001\""));
    }
}

//! Shared test utilities: whole-tree structural equality comparison between
//! two [`XmlDocument`]s.

use docxtpl_xml::{NodeId, NodeKind, XmlDocument};

/// Recursively compare two documents (root tags, attribute sequences,
/// xmlns declarations, child sequences, text/comment/PI values).
pub fn trees_equal(a: &XmlDocument, b: &XmlDocument) -> Result<(), String> {
    compare_node(a, a.root(), b, b.root(), "root")
}

fn compare_node(
    a: &XmlDocument,
    ia: NodeId,
    b: &XmlDocument,
    ib: NodeId,
    path: &str,
) -> Result<(), String> {
    let ka = a.node_kind(ia);
    let kb = b.node_kind(ib);
    if ka != kb {
        return Err(format!("{path}: node kind {ka:?} != {kb:?}"));
    }
    match ka {
        NodeKind::Element => {
            let ta = a.tag(ia).ok_or_else(|| format!("{path}: missing tag"))?;
            let tb = b.tag(ib).ok_or_else(|| format!("{path}: missing tag"))?;
            if ta != tb {
                return Err(format!("{path}: tag {ta:?} != {tb:?}"));
            }
            if a.attrs(ia) != b.attrs(ib) {
                return Err(format!(
                    "{path}: attribute sequences differ: {:?} != {:?}",
                    a.attrs(ia),
                    b.attrs(ib)
                ));
            }
            if a.ns_decls(ia) != b.ns_decls(ib) {
                return Err(format!(
                    "{path}: xmlns declarations differ: {:?} != {:?}",
                    a.ns_decls(ia),
                    b.ns_decls(ib)
                ));
            }
        }
        NodeKind::Text | NodeKind::Comment | NodeKind::Pi => {
            let va = a.node_value(ia);
            let vb = b.node_value(ib);
            if va != vb {
                return Err(format!("{path}: {ka:?} text differs: {va:?} != {vb:?}"));
            }
        }
    }
    let ca = a.children(ia);
    let cb = b.children(ib);
    if ca.len() != cb.len() {
        return Err(format!(
            "{path}: child counts differ: {} != {}",
            ca.len(),
            cb.len()
        ));
    }
    for (i, (xa, xb)) in ca.iter().zip(cb.iter()).enumerate() {
        let label = match a.node_kind(*xa) {
            NodeKind::Element => a
                .tag(*xa)
                .map(|q| format!("{path}/{}", q.local))
                .unwrap_or_else(|| format!("{path}/{i}")),
            _ => format!("{path}/{i}"),
        };
        compare_node(a, *xa, b, *xb, &label)?;
    }
    Ok(())
}

use std::collections::HashMap;
use std::ops::Range;

use docxtpl_xml::{ns_uri, NodeId, XmlDocument};

use crate::{Error, StoryEditor};

/// Resource limits applied while indexing and replacing visible run text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunTextLimits {
    /// Maximum UTF-8 bytes indexed in one paragraph.
    pub max_text_bytes: usize,
    /// Maximum literal matches returned by one search.
    pub max_matches: usize,
    /// Maximum number of UTF-8 bytes a single replacement may add.
    pub max_replacement_growth_bytes: usize,
}

impl Default for RunTextLimits {
    fn default() -> Self {
        Self {
            max_text_bytes: 1024 * 1024,
            max_matches: 10_000,
            max_replacement_growth_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Formatting behavior for a replacement spanning more than one text node.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormattingPolicy {
    /// Put replacement text in the first matched `w:t`, retaining its run
    /// properties, and empty the matched text in later nodes.
    InheritFirstRun,
    /// Reject matches that span more than one `w:r`.
    RequireUniform,
}

/// Mapping from a visible-text range to one `w:t` element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFragment {
    /// The `w:t` element containing this text.
    node: NodeId,
    /// UTF-8 byte range in [`RunTextIndex::text`].
    text_range: Range<usize>,
    /// Original direct text of the element, used to detect a stale index.
    original_text: String,
}

impl TextFragment {
    #[must_use]
    pub const fn node(&self) -> NodeId {
        self.node
    }

    #[must_use]
    pub fn text_range(&self) -> Range<usize> {
        self.text_range.clone()
    }

    #[must_use]
    pub fn original_text(&self) -> &str {
        &self.original_text
    }
}

/// One literal match in a paragraph text index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextMatch {
    paragraph: NodeId,
    /// UTF-8 byte range in [`RunTextIndex::text`].
    text_range: Range<usize>,
    /// Text fragments intersected by the match, in document order.
    fragments: Vec<TextFragment>,
}

impl TextMatch {
    #[must_use]
    pub fn text_range(&self) -> Range<usize> {
        self.text_range.clone()
    }

    #[must_use]
    pub fn fragments(&self) -> &[TextFragment] {
        &self.fragments
    }
}

/// An owned mapping of visible paragraph text back to its `w:t` elements.
///
/// The index deliberately treats non-run children, drawings, fields, tabs,
/// breaks, and container transitions as search boundaries. Literal matches
/// therefore never cross structures that the MVP cannot safely rewrite.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTextIndex {
    story_name: String,
    paragraph: NodeId,
    text: String,
    fragments: Vec<TextFragment>,
    segments: Vec<Range<usize>>,
    limits: RunTextLimits,
}

impl RunTextIndex {
    pub(crate) fn build(
        document: &XmlDocument,
        story_name: &str,
        paragraph: NodeId,
        limits: RunTextLimits,
    ) -> Result<Self, TextIndexError> {
        if !is_word_element(document, paragraph, "p") {
            return Err(TextIndexError::NotParagraph);
        }

        let descendants = document.descendants(paragraph);
        let order: HashMap<NodeId, usize> = descendants
            .iter()
            .copied()
            .enumerate()
            .map(|(index, node)| (node, index))
            .collect();
        let mut raw_segments: Vec<Vec<(NodeId, String)>> = Vec::new();

        for container in descendants {
            let mut current = Vec::new();
            for child in document.children(container).iter().copied() {
                if is_word_element(document, child, "r") {
                    if let Some(texts) = eligible_run_text(document, child) {
                        current.extend(texts);
                    } else {
                        flush_segment(&mut raw_segments, &mut current);
                    }
                } else {
                    flush_segment(&mut raw_segments, &mut current);
                }
            }
            flush_segment(&mut raw_segments, &mut current);
        }

        raw_segments.retain(|segment| !segment.is_empty());
        raw_segments.sort_by_key(|segment| order.get(&segment[0].0).copied().unwrap_or(usize::MAX));

        let mut text = String::new();
        let mut fragments = Vec::new();
        let mut segments = Vec::new();
        for segment in raw_segments {
            let segment_start = text.len();
            for (node, value) in segment {
                if value.is_empty() {
                    continue;
                }
                let start = text.len();
                text.push_str(&value);
                fragments.push(TextFragment {
                    node,
                    text_range: start..text.len(),
                    original_text: value,
                });
                if text.len() > limits.max_text_bytes {
                    return Err(TextIndexError::TextLimit {
                        max: limits.max_text_bytes,
                    });
                }
            }
            if text.len() > segment_start {
                segments.push(segment_start..text.len());
            }
        }

        Ok(Self {
            story_name: story_name.to_string(),
            paragraph,
            text,
            fragments,
            segments,
            limits,
        })
    }

    /// Concatenated visible text. Offsets exposed by this type are UTF-8 byte
    /// offsets into this string.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Paragraph used to create the index.
    #[must_use]
    pub const fn paragraph(&self) -> NodeId {
        self.paragraph
    }

    /// All indexed `w:t` fragments in document order.
    #[must_use]
    pub fn fragments(&self) -> &[TextFragment] {
        &self.fragments
    }

    /// Find non-overlapping literal matches without crossing a safe-rewrite
    /// segment boundary.
    pub fn find_literal(&self, needle: &str) -> Result<Vec<TextMatch>, TextIndexError> {
        if needle.is_empty() {
            return Err(TextIndexError::EmptyNeedle);
        }
        let mut matches = Vec::new();
        for segment in &self.segments {
            let haystack = &self.text[segment.clone()];
            for (offset, _) in haystack.match_indices(needle) {
                if matches.len() == self.limits.max_matches {
                    return Err(TextIndexError::MatchLimit {
                        max: self.limits.max_matches,
                    });
                }
                let text_range = (segment.start + offset)..(segment.start + offset + needle.len());
                let fragments = self
                    .fragments
                    .iter()
                    .filter(|fragment| ranges_intersect(&fragment.text_range, &text_range))
                    .cloned()
                    .collect();
                matches.push(TextMatch {
                    paragraph: self.paragraph,
                    text_range,
                    fragments,
                });
            }
        }
        Ok(matches)
    }
}

impl StoryEditor {
    /// Build a bounded visible-text index for one `w:p` element.
    pub fn run_text_index(
        &self,
        paragraph: NodeId,
        limits: RunTextLimits,
    ) -> Result<RunTextIndex, TextIndexError> {
        RunTextIndex::build(&self.document, &self.name, paragraph, limits)
    }

    /// Replace one match produced by [`RunTextIndex::find_literal`].
    ///
    /// An index is a snapshot. Reusing it after any intersecting `w:t` was
    /// modified returns [`TextIndexError::StaleIndex`].
    pub fn replace_text_match(
        &mut self,
        index: &RunTextIndex,
        matched: &TextMatch,
        replacement: &str,
        formatting: FormattingPolicy,
    ) -> Result<(), TextIndexError> {
        if self.name != index.story_name
            || matched.paragraph != index.paragraph
            || matched.fragments.is_empty()
            || matched.text_range.is_empty()
            || !index.text.is_char_boundary(matched.text_range.start)
            || !index.text.is_char_boundary(matched.text_range.end)
        {
            return Err(TextIndexError::ForeignMatch);
        }
        if !index
            .segments
            .iter()
            .any(|segment| contains_range(segment, &matched.text_range))
        {
            return Err(TextIndexError::ForeignMatch);
        }
        let expected_fragments: Vec<_> = index
            .fragments
            .iter()
            .filter(|fragment| ranges_intersect(&fragment.text_range, &matched.text_range))
            .cloned()
            .collect();
        if expected_fragments != matched.fragments {
            return Err(TextIndexError::ForeignMatch);
        }
        let matched_bytes = matched.text_range.len();
        if replacement.len().saturating_sub(matched_bytes)
            > index.limits.max_replacement_growth_bytes
        {
            return Err(TextIndexError::ReplacementGrowthLimit {
                max: index.limits.max_replacement_growth_bytes,
            });
        }

        for fragment in &matched.fragments {
            if self.document.element_text(fragment.node) != Some(fragment.original_text.as_str()) {
                return Err(TextIndexError::StaleIndex);
            }
        }

        if formatting == FormattingPolicy::RequireUniform {
            let first_run = containing_run(&self.document, matched.fragments[0].node);
            if matched
                .fragments
                .iter()
                .any(|fragment| containing_run(&self.document, fragment.node) != first_run)
            {
                return Err(TextIndexError::FormattingConflict);
            }
        }

        let first = &matched.fragments[0];
        let last = matched.fragments.last().expect("match has fragments");
        let first_start = matched.text_range.start - first.text_range.start;
        let last_end = matched.text_range.end - last.text_range.start;
        if !first.original_text.is_char_boundary(first_start)
            || !last.original_text.is_char_boundary(last_end)
        {
            return Err(TextIndexError::InvalidRange);
        }

        if first.node == last.node {
            let mut value = first.original_text[..first_start].to_string();
            value.push_str(replacement);
            value.push_str(&first.original_text[last_end..]);
            self.document.set_element_text(first.node, &value);
        } else {
            let mut first_value = first.original_text[..first_start].to_string();
            first_value.push_str(replacement);
            self.document.set_element_text(first.node, &first_value);
            for fragment in &matched.fragments[1..matched.fragments.len() - 1] {
                self.document.set_element_text(fragment.node, "");
            }
            self.document
                .set_element_text(last.node, &last.original_text[last_end..]);
        }
        self.changed = true;
        Ok(())
    }
}

/// Stable failures from bounded run-text indexing and replacement.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum TextIndexError {
    #[error("run-text index target is not a w:p element")]
    NotParagraph,
    #[error("literal search needle must not be empty")]
    EmptyNeedle,
    #[error("paragraph visible text exceeds the {max} byte limit")]
    TextLimit { max: usize },
    #[error("literal search exceeds the {max} match limit")]
    MatchLimit { max: usize },
    #[error("replacement growth exceeds the {max} byte limit")]
    ReplacementGrowthLimit { max: usize },
    #[error("text match does not belong to this story index")]
    ForeignMatch,
    #[error("run-text index is stale after an intersecting text edit")]
    StaleIndex,
    #[error("text match spans runs with a uniform-format requirement")]
    FormattingConflict,
    #[error("text match contains an invalid UTF-8 byte range")]
    InvalidRange,
}

impl From<TextIndexError> for Error {
    fn from(error: TextIndexError) -> Self {
        Error::Render(docxtpl_template::RenderError::Template {
            kind: docxtpl_template::TemplateErrorKind::InvalidArgument,
            part: "<postprocess>".to_string(),
            line: None,
            message: error.to_string(),
            context: Vec::new(),
        })
    }
}

fn eligible_run_text(document: &XmlDocument, run: NodeId) -> Option<Vec<(NodeId, String)>> {
    let mut texts = Vec::new();
    for child in document.children(run).iter().copied() {
        if is_word_element(document, child, "rPr") {
            continue;
        }
        if is_word_element(document, child, "t") {
            texts.push((
                child,
                document.element_text(child).unwrap_or_default().to_string(),
            ));
            continue;
        }
        return None;
    }
    (!texts.is_empty()).then_some(texts)
}

fn containing_run(document: &XmlDocument, mut node: NodeId) -> Option<NodeId> {
    while let Some(parent) = document.parent(node) {
        if is_word_element(document, parent, "r") {
            return Some(parent);
        }
        node = parent;
    }
    None
}

fn flush_segment(segments: &mut Vec<Vec<(NodeId, String)>>, current: &mut Vec<(NodeId, String)>) {
    if !current.is_empty() {
        segments.push(std::mem::take(current));
    }
}

fn is_word_element(document: &XmlDocument, node: NodeId, local: &str) -> bool {
    document
        .tag(node)
        .is_some_and(|tag| tag.ns == ns_uri::W && tag.local == local)
}

fn ranges_intersect(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start < right.end && right.start < left.end
}

fn contains_range(outer: &Range<usize>, inner: &Range<usize>) -> bool {
    outer.start <= inner.start && inner.end <= outer.end
}

#[cfg(test)]
mod tests {
    use super::*;
    use docxtpl_xml::XmlLimits;

    const XML: &str = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:rPr><w:b/></w:rPr><w:t>Hello </w:t></w:r><w:r><w:t>world</w:t></w:r><w:bookmarkStart w:id="1" w:name="x"/><w:r><w:t>after</w:t></w:r><w:r><w:tab/><w:t>hidden</w:t></w:r></w:p></w:body></w:document>"#;

    fn parsed() -> (XmlDocument, NodeId) {
        let document = XmlDocument::parse_strict(XML, &XmlLimits::default()).unwrap();
        let paragraph = document
            .descendants(document.root())
            .into_iter()
            .find(|node| is_word_element(&document, *node, "p"))
            .unwrap();
        (document, paragraph)
    }

    #[test]
    fn finds_cross_run_literal_without_crossing_structural_boundaries() {
        let (document, paragraph) = parsed();
        let index = RunTextIndex::build(
            &document,
            "word/document.xml",
            paragraph,
            RunTextLimits::default(),
        )
        .unwrap();

        assert_eq!(index.text(), "Hello worldafter");
        let matches = index.find_literal("lo world").unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].fragments().len(), 2);
        assert!(index.find_literal("worldafter").unwrap().is_empty());
        assert!(index.find_literal("hidden").unwrap().is_empty());
    }

    #[test]
    fn enforces_limits_and_rejects_empty_needles() {
        let (document, paragraph) = parsed();
        let error = RunTextIndex::build(
            &document,
            "word/document.xml",
            paragraph,
            RunTextLimits {
                max_text_bytes: 4,
                ..RunTextLimits::default()
            },
        )
        .unwrap_err();
        assert!(matches!(error, TextIndexError::TextLimit { max: 4 }));

        let index = RunTextIndex::build(
            &document,
            "word/document.xml",
            paragraph,
            RunTextLimits::default(),
        )
        .unwrap();
        assert!(matches!(
            index.find_literal("").unwrap_err(),
            TextIndexError::EmptyNeedle
        ));
    }

    #[test]
    fn replaces_cross_run_text_and_detects_format_conflicts_and_stale_indexes() {
        let (document, paragraph) = parsed();
        let mut story = StoryEditor {
            name: "word/document.xml".to_string(),
            kind: crate::StoryKind::Body,
            document,
            changed: false,
            validate_internal_links: false,
            external_hyperlink_rids: Default::default(),
        };
        let index = story
            .run_text_index(paragraph, RunTextLimits::default())
            .unwrap();
        let matched = index.find_literal("llo world").unwrap().remove(0);

        assert!(matches!(
            story
                .replace_text_match(&index, &matched, "y Rust", FormattingPolicy::RequireUniform,)
                .unwrap_err(),
            TextIndexError::FormattingConflict
        ));
        story
            .replace_text_match(
                &index,
                &matched,
                "y Rust",
                FormattingPolicy::InheritFirstRun,
            )
            .unwrap();
        let updated = story
            .run_text_index(paragraph, RunTextLimits::default())
            .unwrap();
        assert_eq!(updated.text(), "Hey Rustafter");
        assert!(story.is_changed());
        assert!(matches!(
            story
                .replace_text_match(&index, &matched, "again", FormattingPolicy::InheritFirstRun,)
                .unwrap_err(),
            TextIndexError::StaleIndex
        ));
    }
}

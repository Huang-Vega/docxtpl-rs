use std::collections::HashMap;
use std::ops::Range;

use docxtpl_xml::{ns_uri, NodeId, XmlDocument};

use crate::{EditableStoryEditor, Error, StoryEditor};

const REGEX_SIZE_LIMIT: usize = 2 * 1024 * 1024;
const REGEX_DFA_SIZE_LIMIT: usize = 2 * 1024 * 1024;

/// Resource limits applied while indexing and replacing visible run text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunTextLimits {
    /// Maximum UTF-8 bytes indexed in one paragraph.
    pub max_text_bytes: usize,
    /// Maximum literal matches returned by one search.
    pub max_matches: usize,
    /// Maximum UTF-8 byte growth of one replacement operation. For batch
    /// regex replacement this applies to aggregate growth.
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

/// Optional run-property overrides applied to the run receiving replacement
/// text. Existing properties not named here are preserved.
///
/// The builder-style setters are composable. Boolean properties are written
/// explicitly, so `false` overrides an inherited or existing true value.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunFormatOverrides {
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<bool>,
    color: Option<String>,
}

impl RunFormatOverrides {
    /// No formatting overrides.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            bold: None,
            italic: None,
            underline: None,
            color: None,
        }
    }

    /// Override bold formatting.
    #[must_use]
    pub const fn bold(mut self, value: bool) -> Self {
        self.bold = Some(value);
        self
    }

    /// Override italic formatting.
    #[must_use]
    pub const fn italic(mut self, value: bool) -> Self {
        self.italic = Some(value);
        self
    }

    /// Override underline formatting (`single` or `none`).
    #[must_use]
    pub const fn underline(mut self, value: bool) -> Self {
        self.underline = Some(value);
        self
    }

    /// Override text color with six hexadecimal RGB digits or `auto`.
    /// Validation happens when the override is applied.
    #[must_use]
    pub fn color(mut self, value: impl Into<String>) -> Self {
        self.color = Some(value.into());
        self
    }

    const fn is_empty(&self) -> bool {
        self.bold.is_none()
            && self.italic.is_none()
            && self.underline.is_none()
            && self.color.is_none()
    }
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

    /// Find non-overlapping regular-expression matches without crossing a
    /// safe-rewrite segment boundary.
    ///
    /// Compilation uses fixed program and DFA size limits. Paragraph bytes and
    /// returned matches remain bounded by the [`RunTextLimits`] used to build
    /// this index. Patterns producing an empty match are rejected because an
    /// empty range has no unambiguous destination run.
    pub fn find_regex(&self, pattern: &str) -> Result<Vec<TextMatch>, RunTextEditError> {
        let regex = compile_regex(pattern)?;
        let mut matches = Vec::new();
        for segment in &self.segments {
            let haystack = &self.text[segment.clone()];
            for found in regex.find_iter(haystack) {
                if found.is_empty() {
                    return Err(RunTextEditError::EmptyRegexMatch);
                }
                push_match(
                    self,
                    (segment.start + found.start())..(segment.start + found.end()),
                    &mut matches,
                )?;
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

    /// Replace one indexed match and merge composable run-property overrides
    /// into the run receiving the replacement text.
    pub fn replace_text_match_with_format(
        &mut self,
        index: &RunTextIndex,
        matched: &TextMatch,
        replacement: &str,
        formatting: FormattingPolicy,
        overrides: &RunFormatOverrides,
    ) -> Result<(), RunTextEditError> {
        validate_override_values(overrides)?;
        self.replace_text_match(index, matched, replacement, formatting)?;
        if !overrides.is_empty() {
            let run = containing_run(&self.document, matched.fragments[0].node)
                .ok_or(TextIndexError::ForeignMatch)?;
            apply_run_format_overrides(&mut self.document, run, overrides)?;
        }
        Ok(())
    }

    /// Replace every non-overlapping regex match from one index snapshot.
    ///
    /// `$name` and `$1` capture expansion follows the `regex` crate syntax.
    /// All matches, growth limits, stale fragments, formatting requirements,
    /// and overrides are validated before the DOM is changed.
    pub fn replace_regex_all(
        &mut self,
        index: &RunTextIndex,
        pattern: &str,
        replacement: &str,
        formatting: FormattingPolicy,
        overrides: &RunFormatOverrides,
    ) -> Result<usize, RunTextEditError> {
        if self.name != index.story_name {
            return Err(TextIndexError::ForeignMatch.into());
        }
        let regex = compile_regex(pattern)?;
        let mut replacements = Vec::new();
        let mut total_growth = 0usize;
        for segment in &index.segments {
            let haystack = &index.text[segment.clone()];
            for captures in regex.captures_iter(haystack) {
                if replacements.len() == index.limits.max_matches {
                    return Err(TextIndexError::MatchLimit {
                        max: index.limits.max_matches,
                    }
                    .into());
                }
                let found = captures.get(0).expect("regex capture zero exists");
                if found.is_empty() {
                    return Err(RunTextEditError::EmptyRegexMatch);
                }
                let text_range = (segment.start + found.start())..(segment.start + found.end());
                let expansion_upper_bound = replacement.len().saturating_add(
                    replacement
                        .bytes()
                        .filter(|byte| *byte == b'$')
                        .count()
                        .saturating_mul(haystack.len()),
                );
                if expansion_upper_bound.saturating_sub(found.len())
                    > index.limits.max_replacement_growth_bytes
                {
                    return Err(TextIndexError::ReplacementGrowthLimit {
                        max: index.limits.max_replacement_growth_bytes,
                    }
                    .into());
                }
                let mut matches = Vec::with_capacity(1);
                push_match(index, text_range, &mut matches)?;
                let matched = matches.pop().expect("one pushed regex match");
                let mut expanded = String::new();
                captures.expand(replacement, &mut expanded);
                validate_replacement(self, index, &matched, &expanded, formatting)?;
                total_growth = total_growth
                    .saturating_add(expanded.len().saturating_sub(matched.text_range.len()));
                if total_growth > index.limits.max_replacement_growth_bytes {
                    return Err(TextIndexError::ReplacementGrowthLimit {
                        max: index.limits.max_replacement_growth_bytes,
                    }
                    .into());
                }
                replacements.push((matched, expanded));
            }
        }
        validate_override_values(overrides)?;
        if replacements.is_empty() {
            return Ok(0);
        }

        let mut updated_fragments = Vec::new();
        for fragment in &index.fragments {
            let mut value = String::new();
            let mut cursor = fragment.text_range.start;
            for (matched, replacement) in &replacements {
                if !ranges_intersect(&fragment.text_range, &matched.text_range) {
                    continue;
                }
                let preserved_end = matched
                    .text_range
                    .start
                    .clamp(cursor, fragment.text_range.end);
                value.push_str(
                    &fragment.original_text[(cursor - fragment.text_range.start)
                        ..(preserved_end - fragment.text_range.start)],
                );
                if matched.fragments[0].node == fragment.node {
                    value.push_str(replacement);
                }
                cursor = cursor.max(matched.text_range.end.min(fragment.text_range.end));
            }
            if cursor != fragment.text_range.start {
                value.push_str(
                    &fragment.original_text[(cursor - fragment.text_range.start)
                        ..(fragment.text_range.end - fragment.text_range.start)],
                );
                updated_fragments.push((fragment.node, value));
            }
        }

        for (node, value) in updated_fragments {
            self.document.set_element_text(node, &value);
        }
        if !overrides.is_empty() {
            let mut formatted_runs = Vec::new();
            for (matched, _) in &replacements {
                let run = containing_run(&self.document, matched.fragments[0].node)
                    .ok_or(TextIndexError::ForeignMatch)?;
                if !formatted_runs.contains(&run) {
                    apply_run_format_overrides(&mut self.document, run, overrides)?;
                    formatted_runs.push(run);
                }
            }
        }
        self.changed = true;
        Ok(replacements.len())
    }
}

impl EditableStoryEditor {
    /// Build a bounded visible-text index for one `w:p` element in any
    /// unified editable story.
    pub fn run_text_index(
        &self,
        paragraph: NodeId,
        limits: RunTextLimits,
    ) -> Result<RunTextIndex, TextIndexError> {
        self.inner.run_text_index(paragraph, limits)
    }

    /// Replace one match produced by [`RunTextIndex::find_literal`].
    pub fn replace_text_match(
        &mut self,
        index: &RunTextIndex,
        matched: &TextMatch,
        replacement: &str,
        formatting: FormattingPolicy,
    ) -> Result<(), TextIndexError> {
        self.inner
            .replace_text_match(index, matched, replacement, formatting)
    }

    /// Replace one indexed match and merge run-property overrides.
    pub fn replace_text_match_with_format(
        &mut self,
        index: &RunTextIndex,
        matched: &TextMatch,
        replacement: &str,
        formatting: FormattingPolicy,
        overrides: &RunFormatOverrides,
    ) -> Result<(), RunTextEditError> {
        self.inner.replace_text_match_with_format(
            index,
            matched,
            replacement,
            formatting,
            overrides,
        )
    }

    /// Replace every non-overlapping regex match from one index snapshot.
    pub fn replace_regex_all(
        &mut self,
        index: &RunTextIndex,
        pattern: &str,
        replacement: &str,
        formatting: FormattingPolicy,
        overrides: &RunFormatOverrides,
    ) -> Result<usize, RunTextEditError> {
        self.inner
            .replace_regex_all(index, pattern, replacement, formatting, overrides)
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

/// Failures added by the 1.3 regex and composable-formatting APIs.
///
/// Existing index and replacement failures are preserved in [`Self::Index`]
/// without extending the exhaustively matchable 1.2 [`TextIndexError`] enum.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RunTextEditError {
    #[error(transparent)]
    Index(#[from] TextIndexError),
    #[error("regular expression is invalid or exceeds its compile limits: {message}")]
    InvalidRegex { message: String },
    #[error("regular expressions that produce empty matches are not supported")]
    EmptyRegexMatch,
    #[error("run text color must be six hexadecimal RGB digits or 'auto': {value:?}")]
    InvalidColor { value: String },
    #[error("run formatting override could not be represented: {message}")]
    RunFormat { message: String },
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

impl From<RunTextEditError> for Error {
    fn from(error: RunTextEditError) -> Self {
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

fn compile_regex(pattern: &str) -> Result<regex::Regex, RunTextEditError> {
    regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .dfa_size_limit(REGEX_DFA_SIZE_LIMIT)
        .build()
        .map_err(|error| RunTextEditError::InvalidRegex {
            message: error.to_string(),
        })
}

fn push_match(
    index: &RunTextIndex,
    text_range: Range<usize>,
    matches: &mut Vec<TextMatch>,
) -> Result<(), TextIndexError> {
    if matches.len() == index.limits.max_matches {
        return Err(TextIndexError::MatchLimit {
            max: index.limits.max_matches,
        });
    }
    let fragments = index
        .fragments
        .iter()
        .filter(|fragment| ranges_intersect(&fragment.text_range, &text_range))
        .cloned()
        .collect();
    matches.push(TextMatch {
        paragraph: index.paragraph,
        text_range,
        fragments,
    });
    Ok(())
}

fn validate_replacement(
    story: &StoryEditor,
    index: &RunTextIndex,
    matched: &TextMatch,
    replacement: &str,
    formatting: FormattingPolicy,
) -> Result<(), TextIndexError> {
    if story.name != index.story_name
        || matched.paragraph != index.paragraph
        || matched.fragments.is_empty()
        || matched.text_range.is_empty()
        || !index.text.is_char_boundary(matched.text_range.start)
        || !index.text.is_char_boundary(matched.text_range.end)
        || !index
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
    if replacement.len().saturating_sub(matched.text_range.len())
        > index.limits.max_replacement_growth_bytes
    {
        return Err(TextIndexError::ReplacementGrowthLimit {
            max: index.limits.max_replacement_growth_bytes,
        });
    }
    for fragment in &matched.fragments {
        if story.document.element_text(fragment.node) != Some(fragment.original_text.as_str()) {
            return Err(TextIndexError::StaleIndex);
        }
    }
    if formatting == FormattingPolicy::RequireUniform {
        let first_run = containing_run(&story.document, matched.fragments[0].node);
        if matched
            .fragments
            .iter()
            .any(|fragment| containing_run(&story.document, fragment.node) != first_run)
        {
            return Err(TextIndexError::FormattingConflict);
        }
    }
    Ok(())
}

fn validate_override_values(overrides: &RunFormatOverrides) -> Result<(), RunTextEditError> {
    if let Some(color) = &overrides.color {
        if color != "auto"
            && (color.len() != 6 || !color.bytes().all(|byte| byte.is_ascii_hexdigit()))
        {
            return Err(RunTextEditError::InvalidColor {
                value: color.clone(),
            });
        }
    }
    Ok(())
}

fn apply_run_format_overrides(
    document: &mut XmlDocument,
    run: NodeId,
    overrides: &RunFormatOverrides,
) -> Result<(), RunTextEditError> {
    validate_override_values(overrides)?;
    let rpr = if let Some(existing) = document
        .children(run)
        .iter()
        .copied()
        .find(|node| is_word_element(document, *node, "rPr"))
    {
        existing
    } else {
        let created = document.new_w_element("rPr", Vec::new()).map_err(|error| {
            RunTextEditError::RunFormat {
                message: error.to_string(),
            }
        })?;
        document.insert_child_at(run, 0, created);
        created
    };

    if let Some(value) = overrides.bold {
        set_run_property(document, rpr, "b", if value { "1" } else { "0" })?;
    }
    if let Some(value) = overrides.italic {
        set_run_property(document, rpr, "i", if value { "1" } else { "0" })?;
    }
    if let Some(value) = overrides.underline {
        set_run_property(document, rpr, "u", if value { "single" } else { "none" })?;
    }
    if let Some(value) = &overrides.color {
        set_run_property(document, rpr, "color", value)?;
    }
    Ok(())
}

fn set_run_property(
    document: &mut XmlDocument,
    rpr: NodeId,
    local: &str,
    value: &str,
) -> Result<(), RunTextEditError> {
    if let Some(existing) = document
        .children(rpr)
        .iter()
        .copied()
        .find(|node| is_word_element(document, *node, local))
    {
        document.set_attr(existing, ns_uri::W, "val", value.to_string());
        return Ok(());
    }
    let property = document
        .new_w_element(local, vec![("val".to_string(), value.to_string())])
        .map_err(|error| RunTextEditError::RunFormat {
            message: error.to_string(),
        })?;
    let property_rank = run_property_rank(local);
    let insertion = document
        .children(rpr)
        .iter()
        .position(|child| {
            document
                .tag(*child)
                .filter(|tag| tag.ns == ns_uri::W)
                .is_some_and(|tag| run_property_rank(&tag.local) > property_rank)
        })
        .unwrap_or(document.children(rpr).len());
    document.insert_child_at(rpr, insertion, property);
    Ok(())
}

fn run_property_rank(local: &str) -> usize {
    match local {
        "rStyle" => 0,
        "rFonts" => 1,
        "b" => 2,
        "bCs" => 3,
        "i" => 4,
        "iCs" => 5,
        "caps" => 6,
        "smallCaps" => 7,
        "strike" => 8,
        "dstrike" => 9,
        "outline" => 10,
        "shadow" => 11,
        "emboss" => 12,
        "imprint" => 13,
        "noProof" => 14,
        "snapToGrid" => 15,
        "vanish" => 16,
        "webHidden" => 17,
        "color" => 18,
        "spacing" => 19,
        "w" => 20,
        "kern" => 21,
        "position" => 22,
        "sz" => 23,
        "szCs" => 24,
        "highlight" => 25,
        "u" => 26,
        "effect" => 27,
        "bdr" => 28,
        "shd" => 29,
        "fitText" => 30,
        "vertAlign" => 31,
        "rtl" => 32,
        "cs" => 33,
        "em" => 34,
        "lang" => 35,
        "eastAsianLayout" => 36,
        "specVanish" => 37,
        "oMath" => 38,
        _ => usize::MAX,
    }
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
        assert!(matches!(
            index.find_regex("(").unwrap_err(),
            RunTextEditError::InvalidRegex { .. }
        ));
        assert_eq!(
            index.find_regex("^").unwrap_err(),
            RunTextEditError::EmptyRegexMatch
        );

        let limited = RunTextIndex::build(
            &document,
            "word/document.xml",
            paragraph,
            RunTextLimits {
                max_matches: 1,
                ..RunTextLimits::default()
            },
        )
        .unwrap();
        assert_eq!(
            limited.find_regex("l").unwrap_err(),
            RunTextEditError::Index(TextIndexError::MatchLimit { max: 1 })
        );
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
            image_rids: Default::default(),
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

    #[test]
    fn replaces_regex_captures_and_merges_run_format_overrides() {
        let (document, paragraph) = parsed();
        let mut story = StoryEditor {
            name: "word/document.xml".to_string(),
            kind: crate::StoryKind::Body,
            document,
            changed: false,
            validate_internal_links: false,
            external_hyperlink_rids: Default::default(),
            image_rids: Default::default(),
        };
        let index = story
            .run_text_index(paragraph, RunTextLimits::default())
            .unwrap();
        let overrides = RunFormatOverrides::new()
            .bold(false)
            .italic(true)
            .underline(true)
            .color("00aaFF");

        assert_eq!(
            story
                .replace_regex_all(
                    &index,
                    r"(Hello) (world)",
                    "$2, $1",
                    FormattingPolicy::InheritFirstRun,
                    &overrides,
                )
                .unwrap(),
            1
        );
        let updated = story
            .run_text_index(paragraph, RunTextLimits::default())
            .unwrap();
        assert_eq!(updated.text(), "world, Helloafter");
        let xml = story.document.try_serialize(1024 * 1024).unwrap();
        assert!(xml.contains("<w:b w:val=\"0\""));
        assert!(xml.contains("<w:i w:val=\"1\""));
        assert!(xml.contains("<w:color w:val=\"00aaFF\""));
        assert!(xml.contains("<w:u w:val=\"single\""));
    }

    #[test]
    fn regex_batch_replaces_multiple_matches_from_one_snapshot() {
        let (document, paragraph) = parsed();
        let mut story = StoryEditor {
            name: "word/document.xml".to_string(),
            kind: crate::StoryKind::Body,
            document,
            changed: false,
            validate_internal_links: false,
            external_hyperlink_rids: Default::default(),
            image_rids: Default::default(),
        };
        let index = story
            .run_text_index(paragraph, RunTextLimits::default())
            .unwrap();
        let count = story
            .replace_regex_all(
                &index,
                "[aeiou]",
                "_",
                FormattingPolicy::InheritFirstRun,
                &RunFormatOverrides::new(),
            )
            .unwrap();
        assert_eq!(count, 5);
        assert_eq!(
            story
                .run_text_index(paragraph, RunTextLimits::default())
                .unwrap()
                .text(),
            "H_ll_ w_rld_ft_r"
        );
    }

    #[test]
    fn invalid_format_override_does_not_modify_text() {
        let (document, paragraph) = parsed();
        let mut story = StoryEditor {
            name: "word/document.xml".to_string(),
            kind: crate::StoryKind::Body,
            document,
            changed: false,
            validate_internal_links: false,
            external_hyperlink_rids: Default::default(),
            image_rids: Default::default(),
        };
        let index = story
            .run_text_index(paragraph, RunTextLimits::default())
            .unwrap();
        let matched = index.find_literal("Hello").unwrap().remove(0);
        let error = story
            .replace_text_match_with_format(
                &index,
                &matched,
                "changed",
                FormattingPolicy::InheritFirstRun,
                &RunFormatOverrides::new().color("not-a-color"),
            )
            .unwrap_err();
        assert!(matches!(error, RunTextEditError::InvalidColor { .. }));
        assert_eq!(
            story.document.element_text(matched.fragments[0].node),
            Some("Hello ")
        );
        assert!(!story.is_changed());
    }

    #[test]
    fn regex_growth_limit_rejects_the_batch_before_mutation() {
        let (document, paragraph) = parsed();
        let mut story = StoryEditor {
            name: "word/document.xml".to_string(),
            kind: crate::StoryKind::Body,
            document,
            changed: false,
            validate_internal_links: false,
            external_hyperlink_rids: Default::default(),
            image_rids: Default::default(),
        };
        let index = story
            .run_text_index(
                paragraph,
                RunTextLimits {
                    max_replacement_growth_bytes: 1,
                    ..RunTextLimits::default()
                },
            )
            .unwrap();
        let error = story
            .replace_regex_all(
                &index,
                "[aeiou]",
                "___",
                FormattingPolicy::InheritFirstRun,
                &RunFormatOverrides::new(),
            )
            .unwrap_err();
        assert_eq!(
            error,
            RunTextEditError::Index(TextIndexError::ReplacementGrowthLimit { max: 1 })
        );
        assert_eq!(
            story.document.element_text(index.fragments[0].node),
            Some("Hello ")
        );
        assert!(!story.is_changed());
    }
}

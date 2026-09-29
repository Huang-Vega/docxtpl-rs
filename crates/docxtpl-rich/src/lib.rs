//! docxtpl-rich: the P4 rich-content value library.
//!
//! Aligned with the upstream baselines **docxtpl 0.20.2** (`richtext.py` / `listing.py` /
//! `inline_image.py`) and **python-docx 1.2.0** (`docx/image/*` image header parsing); see
//! ADR-005 for design and decisions. This crate only provides typed rich-content values and
//! string generation; it has no dependency on XML/OPC/the template engine. Package-level side
//! effects such as writing image parts and allocating rIds are handled by the layer above
//! (docxtpl-rs's ImageRegistry).
//!
//! Pinned semantic points (verified by probes):
//! - Text escaping in RichText / Listing matches the default behavior of Python
//!   `html.escape(text)` (quote=True: all five characters `& < > " '` are escaped);
//! - The RichText constructor skips empty text per the upstream `if text:` falsy check, while
//!   `add()` emits an empty run for the empty string (upstream has that check commented out);
//! - Size conversion `int((px/dpi)*914400)` truncates toward zero, and one-sided scaling uses
//!   Python round's banker's rounding ([`py_round`]);
//! - Signature matching and field reads for image headers are replicated field by field from
//!   python-docx, with all truncation/missing-segment failures collapsed into
//!   [`ImageError::Unrecognized`];
//! - The output of [`render_inline_image`] matches the upstream pretty-serialization probe
//!   character for character (including split-run wrapping and 2-space-per-level indentation).

mod image;
mod inline_image;
mod listing;
mod richtext;

pub use image::{
    probe, probe_with_digest, py_round, sha1_digest, ImageDigest, ImageError, ImageInfo,
};
pub use inline_image::{
    render_inline_image, render_inline_image_with_info, scaled_dimensions, InlineImage,
    InlineImageLoadError, LazyImageFile,
};
pub use listing::Listing;
pub use richtext::{RichText, RichTextParagraph, RichTextProps};

//! docxtpl-rich：P4 富内容值库。
//!
//! 对齐上游基线 **docxtpl 0.20.2**（`richtext.py` / `listing.py` /
//! `inline_image.py`）与 **python-docx 1.2.0**（`docx/image/*` 图片头解析），
//! 设计与决策见 ADR-005。本 crate 只提供类型化富内容值与字符串生成，
//! 不依赖 XML/OPC/模板引擎；图片 part 写入、rId 分配等包级副作用由上层
//! （docxtpl-rs 的 ImageRegistry）完成。
//!
//! 语义钉死要点（探针实证）：
//! - RichText / Listing 的文本转义对齐 Python `html.escape(text)` 的默认
//!   行为（quote=True：`& < > " '` 五个字符都转义）；
//! - RichText 构造函数对空文本按上游 `if text:` falsy 跳过，而 `add()`
//!   对空串输出空 run（上游注释掉了该检查）；
//! - 尺寸换算 `int((px/dpi)*914400)` 向零截断，单边缩放按 Python round 的
//!   银行家舍入（[`py_round`]）；
//! - 图片头的签名匹配与字段读取逐字段复刻 python-docx，各种截断/缺段
//!   失败统一归并为 [`ImageError::Unrecognized`]；
//! - [`render_inline_image`] 的输出与上游 pretty 序列化探针逐字符一致
//!   （含拆 run 包装与 2 空格/层缩进）。

mod image;
mod inline_image;
mod listing;
mod richtext;

pub use image::{probe, py_round, ImageError, ImageInfo};
pub use inline_image::{render_inline_image, scaled_dimensions, InlineImage};
pub use listing::Listing;
pub use richtext::{RichText, RichTextParagraph, RichTextProps};

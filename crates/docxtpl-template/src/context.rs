//! P4 富内容渲染上下文（ADR-005）。
//!
//! [`RenderContext`] 是有序的顶层变量表，值可以是：
//!
//! - 纯 JSON（[`serde_json::Value`]）：与 P2/P3 的 JSON 路径完全同构；
//! - 富内容：[`RichText`] / [`RichTextParagraph`] / [`Listing`] /
//!   [`InlineImage`]（docxtpl-rich）；
//! - 嵌套数组 / 对象（支撑 `{% for row in rows %}` 中含富值的场景）。
//!
//! 富文本/列表在渲染期直接以其 `to_xml()` 字符串参与 MiniJinja 渲染
//! （对齐上游 `RichText.__str__` / `Listing.__str__`）；图片则先经
//! [`ImageRegistry`] 解析出关系 ID，再生成 `wp:inline` XML。
//!
//! 注意：图片按上游 `InlineImage.__str__` 语义**惰性解析**（P5/ADR-006
//! 修订 P4 的 eager 方案）：转换期只写占位符，渲染后按占位符在输出中的
//! 出现顺序经 [`ImageRegistry`] 解析。因此未被某 part 模板引用的图片不会
//! 在该 part 产生关系（多 part 作用域正确性的关键），未被任何模板引用的
//! 坏图片也不会报错（与上游一致）。

use docxtpl_rich::{InlineImage, Listing, RichText, RichTextParagraph};
use serde_json::Value as JsonValue;

/// 一张图片解析得到的关系 ID（由 [`ImageRegistry`] 分配）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRels {
    /// `a:blip r:embed` 的图片关系 ID。
    pub blip_rid: String,
    /// 锚点外部超链接关系 ID；无锚点时为 `None`。
    pub hyperlink_rid: Option<String>,
}

/// 图片注册表：把 [`InlineImage`] 解析为关系 ID（ADR-005）。
///
/// 模板 crate 不依赖 OPC 包层；具体实现（part 命名、sha1 去重、rId 空洞
/// 回填、外链复用）在 docxtpl-rs。同一图片多次解析必须幂等（对齐上游
/// `get_or_add_image` / `get_or_add_ext_rel`）。
pub trait ImageRegistry {
    /// 解析图片：探测头/换算尺寸/分配 part 与关系。
    ///
    /// 图片字节无法识别时返回 [`ImageResolveError`]，由渲染管线归并为
    /// `TemplateErrorKind::Image`（oracle 异常 `UnrecognizedImageError`）。
    fn resolve_image(&mut self, image: &InlineImage) -> Result<ImageRels, ImageResolveError>;
}

/// 图片注册失败：仅承载消息（包细节属于上层）。
#[derive(Debug, Clone)]
pub struct ImageResolveError {
    /// 失败原因（如 docxtpl-rich 的 `ImageError` 显示文本）。
    pub message: String,
}

impl std::fmt::Display for ImageResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ImageResolveError {}

/// 空注册表：纯 JSON 上下文路径不可能出现图片，调用即失败（防御性）。
pub struct NullRegistry;

impl ImageRegistry for NullRegistry {
    fn resolve_image(&mut self, _image: &InlineImage) -> Result<ImageRels, ImageResolveError> {
        Err(ImageResolveError {
            message: "JSON 上下文不支持 InlineImage；请使用 render_ctx 与富内容上下文".to_string(),
        })
    }
}

/// 渲染上下文中的一个值（JSON 或富内容，可嵌套）。
#[derive(Debug, Clone)]
pub enum RenderValue {
    /// 纯 JSON 值（字符串/数字/布尔/null/数组/对象）。
    Json(JsonValue),
    /// 富文本 run 序列。
    RichText(RichText),
    /// 富文本独立段落。
    RichTextParagraph(RichTextParagraph),
    /// 转义纯文本（控制符由管线 resolve_listing 展开）。
    Listing(Listing),
    /// 内联图片。
    Image(InlineImage),
    /// 有序数组。
    Array(Vec<RenderValue>),
    /// 有序对象（键值对保序）。
    Object(Vec<(String, RenderValue)>),
}

impl RenderValue {
    /// 构造有序对象值。
    #[must_use]
    pub fn object(entries: Vec<(String, RenderValue)>) -> Self {
        Self::Object(entries)
    }

    /// 构造有序数组值。
    #[must_use]
    pub fn array(items: Vec<RenderValue>) -> Self {
        Self::Array(items)
    }
}

impl From<JsonValue> for RenderValue {
    fn from(value: JsonValue) -> Self {
        Self::Json(value)
    }
}

impl From<&str> for RenderValue {
    fn from(value: &str) -> Self {
        Self::Json(JsonValue::String(value.to_owned()))
    }
}

impl From<String> for RenderValue {
    fn from(value: String) -> Self {
        Self::Json(JsonValue::String(value))
    }
}

impl From<bool> for RenderValue {
    fn from(value: bool) -> Self {
        Self::Json(JsonValue::Bool(value))
    }
}

impl From<i64> for RenderValue {
    fn from(value: i64) -> Self {
        Self::Json(JsonValue::from(value))
    }
}

impl From<f64> for RenderValue {
    fn from(value: f64) -> Self {
        Self::Json(JsonValue::from(value))
    }
}

impl From<RichText> for RenderValue {
    fn from(value: RichText) -> Self {
        Self::RichText(value)
    }
}

impl From<RichTextParagraph> for RenderValue {
    fn from(value: RichTextParagraph) -> Self {
        Self::RichTextParagraph(value)
    }
}

impl From<Listing> for RenderValue {
    fn from(value: Listing) -> Self {
        Self::Listing(value)
    }
}

impl From<InlineImage> for RenderValue {
    fn from(value: InlineImage) -> Self {
        Self::Image(value)
    }
}

impl From<Vec<RenderValue>> for RenderValue {
    fn from(value: Vec<RenderValue>) -> Self {
        Self::Array(value)
    }
}

/// 顶层渲染上下文：有序键值表（对齐 Python `dict` 的插入序）。
#[derive(Debug, Clone, Default)]
pub struct RenderContext {
    entries: Vec<(String, RenderValue)>,
}

impl RenderContext {
    /// 空上下文。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 插入/覆盖一个顶层变量（链式）。
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<RenderValue>) -> &mut Self {
        let key = key.into();
        let value = value.into();
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
        self
    }

    /// 顶层变量数。
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// 是否为空。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 按键读取。
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&RenderValue> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// 按插入序迭代。
    pub fn iter(&self) -> impl Iterator<Item = (&str, &RenderValue)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// 从 JSON 对象构造（非对象值得到空上下文，与上游要求 dict 一致）。
    #[must_use]
    pub fn from_json(value: &JsonValue) -> Self {
        let mut ctx = Self::new();
        if let Some(map) = value.as_object() {
            for (k, v) in map {
                ctx.entries.push((k.clone(), json_to_value(v)));
            }
        }
        ctx
    }
}

/// 递归把 JSON 值映射为 [`RenderValue`]（object/array 保序）。
fn json_to_value(value: &JsonValue) -> RenderValue {
    match value {
        JsonValue::Array(items) => RenderValue::Array(items.iter().map(json_to_value).collect()),
        JsonValue::Object(map) => RenderValue::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect(),
        ),
        other => RenderValue::Json(other.clone()),
    }
}

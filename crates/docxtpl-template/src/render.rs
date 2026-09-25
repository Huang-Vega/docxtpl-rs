//! 渲染主管线：patch → MiniJinja → resolve_listing → recover → fix → serialize。

use docxtpl_compat::{patch_xml, resolve_listing};
use docxtpl_xml::{Recovery, XmlDocument, XmlLimits};
use minijinja::{Environment, ErrorKind, UndefinedBehavior, Value};
use regex::Regex;
use serde_json::Value as JsonValue;
use std::io::{self, Write};
use std::sync::OnceLock;

use crate::context::{ImageRegistry, ImageResolveError, NullRegistry, RenderContext, RenderValue};
use crate::error::{RenderError, TemplateErrorKind};
use crate::fix_tables::{fix_docpr_ids, fix_tables};

const MAX_RENDERED_XML_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEMPLATE_FUEL: u64 = 10_000_000;

struct LimitedOutput {
    bytes: Vec<u8>,
    max: usize,
    exceeded: bool,
}

impl Write for LimitedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.len() > self.max.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(io::Error::other("rendered XML size limit exceeded"));
        }
        self.bytes.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 主文档 part 名（本阶段仅渲染主文档，header/footer 属 P5）。
pub const MAIN_PART: &str = "word/document.xml";

/// 渲染选项（代码规范 §3.3：影响输出的选项必须显式传入，不读环境变量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOptions {
    /// 是否自动转义。上游 `DocxTemplate.render(autoescape=False)` 默认 false。
    autoescape: bool,
}

impl RenderOptions {
    /// 与上游默认行为一致的兼容模式：`autoescape=false`、lenient undefined。
    #[must_use]
    pub fn compat() -> Self {
        Self { autoescape: false }
    }

    /// 开启 autoescape 的显式增强（独立选项，不影响兼容口径，ADR-003）。
    #[must_use]
    pub fn with_autoescape(mut self, enabled: bool) -> Self {
        self.autoescape = enabled;
        self
    }

    /// 当前 autoescape 设置。
    #[must_use]
    pub fn autoescape(self) -> bool {
        self.autoescape
    }
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self::compat()
    }
}

/// 一次渲染的结果：新 part XML 与 recover 愈合诊断（ADR-002：愈合不静默）。
#[derive(Debug)]
pub struct RenderOutcome {
    /// 渲染并后处理后的 XML 字符串。
    pub xml: String,
    /// recover 解析记录的全部愈合动作。
    pub recoveries: Vec<Recovery>,
}

/// `<w:p>` 前插换行（上游 render_xml_part 起手，仅为错误行号定位）。
fn newline_before_p() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<w:p([ >])").expect("invalid regex"))
}

/// 渲染后移除插入的换行。
fn newline_before_p_remove() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\n<w:p([ >])").expect("invalid regex"))
}

/// 去 XML 标签（对齐上游错误上下文 `re.sub(r"<[^>]+>", "", line)`）。
fn strip_xml_tags() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]+>").expect("invalid regex"))
}

/// 渲染一个 XML part（当前阶段即 word/document.xml）。
///
/// 输入是模板 part 原文，输出是可写回的 XML；语义见
/// docs/compatibility.md §2 与 ADR-002/003。
pub fn render_document_xml(
    src_xml: &str,
    context: &JsonValue,
    options: &RenderOptions,
) -> Result<RenderOutcome, RenderError> {
    let ctx = RenderContext::from_json(context);
    let mut null_registry = NullRegistry;
    render_document_xml_ctx(src_xml, &ctx, options, &mut null_registry)
}

/// 富内容版本的主文档渲染（P4，ADR-005）：上下文可含 RichText /
/// RichTextParagraph / Listing / InlineImage，图片由 `registry` 解析。
pub fn render_document_xml_ctx(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
) -> Result<RenderOutcome, RenderError> {
    // 0. shape_id：上游对渲染前的原始 document 树执行 next_id
    // （xpath("//@id")：只命中无命名空间前缀的 id 属性，w:id/r:id 不命中），
    // 取纯数字 id 的最大值 +1，无则 1。字符串渲染不改树，多图共享同一 id。
    let shape_id = next_shape_id(src_xml);

    // 1. patch_xml：13 步有界正则变换。
    let patched = patch_xml(src_xml);

    // 2. 每个段落前插换行（仅用于错误定位，渲染后撤销）。
    let prepared = newline_before_p()
        .replace_all(&patched, "\n<w:p${1}")
        .into_owned();

    // 3. MiniJinja 渲染（默认对齐 jinja2：lenient undefined、autoescape=false）。
    //    富值在此处整体转换（图片解析/关系分配随之发生）。
    let env = build_jinja_env(options.autoescape());
    let root = context_to_minijinja(context, registry, shape_id, MAIN_PART)?;
    let rendered = render_inline_value(&env, &prepared, root, MAIN_PART)?;

    // 4. 撤销换行 + 还原 {_{ }_} 字面转义。
    let dst = newline_before_p_remove()
        .replace_all(&rendered, "<w:p${1}")
        .into_owned();
    let dst = dst
        .replace("{_{", "{{")
        .replace("}_}", "}}")
        .replace("{%_", "{%")
        .replace("%_}", "%}");

    // 5. resolve_listing：\n \t \a \f → br/tab/换段/分页。
    let dst = resolve_listing(&dst);
    if dst.len() > MAX_RENDERED_XML_BYTES {
        return Err(RenderError::Limit {
            part: MAIN_PART.to_string(),
            kind: "rendered_xml_bytes",
            max: MAX_RENDERED_XML_BYTES as u64,
        });
    }

    // 6. 宽松(recover)解析愈合（安全限额仍强制）。
    let outcome =
        XmlDocument::parse_lenient(&dst, &XmlLimits::default()).map_err(|e| RenderError::Xml {
            part: MAIN_PART.to_string(),
            source: e,
        })?;
    let mut doc = outcome.doc;

    // 7. fix_tables / fix_docpr_ids。
    fix_tables(&mut doc)?;
    fix_docpr_ids(&mut doc);

    Ok(RenderOutcome {
        xml: doc.serialize(),
        recoveries: outcome.diagnostics,
    })
}

/// 上游 `next_id`：原始 XML 中无前缀 `id="数字"` 属性的最大值 +1（无则 1）。
///
/// 正则要求 `id` 前是空白，因此 `w:id` / `r:id`（冒号紧贴）不会误匹配。
fn next_shape_id(src_xml: &str) -> i64 {
    fn shape_id_re() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| Regex::new(r#"\sid="([0-9]+)""#).expect("invalid regex"))
    }
    let mut max_id: i64 = 0;
    for captures in shape_id_re().captures_iter(src_xml) {
        if let Some(digits) = captures.get(1) {
            if let Ok(value) = digits.as_str().parse::<i64>() {
                max_id = max_id.max(value);
            }
        }
    }
    max_id + 1
}

/// 把 [`RenderContext`] 转换为 MiniJinja 根值。
pub(crate) fn context_to_minijinja(
    context: &RenderContext,
    registry: &mut dyn ImageRegistry,
    shape_id: i64,
    part: &str,
) -> Result<Value, RenderError> {
    let mut pairs = Vec::with_capacity(context.len());
    for (key, value) in context.iter() {
        pairs.push((
            Value::from(key),
            value_to_minijinja(value, registry, shape_id, part)?,
        ));
    }
    Ok(Value::from_iter(pairs))
}

/// 递归把 [`RenderValue`] 转换为 MiniJinja 值。
///
/// RichText/RichTextParagraph/Listing 直接给出生成的 XML 字符串（对齐上游
/// `__str__`，autoescape 缺省关闭）；InlineImage 经注册表解析后生成
/// `wp:inline` 字符串。
fn value_to_minijinja(
    value: &RenderValue,
    registry: &mut dyn ImageRegistry,
    shape_id: i64,
    part: &str,
) -> Result<Value, RenderError> {
    match value {
        RenderValue::Json(json) => Ok(Value::from_serialize(json)),
        RenderValue::RichText(rich) => Ok(Value::from(rich.to_xml())),
        RenderValue::RichTextParagraph(paragraph) => Ok(Value::from(paragraph.to_xml())),
        RenderValue::Listing(listing) => Ok(Value::from(listing.to_xml())),
        RenderValue::Image(image) => {
            let rels = registry
                .resolve_image(image)
                .map_err(|err| image_error(&err, part))?;
            let xml = docxtpl_rich::render_inline_image(
                image,
                shape_id,
                &rels.blip_rid,
                rels.hyperlink_rid.as_deref(),
            )
            .map_err(|err| {
                image_error(
                    &ImageResolveError {
                        message: err.to_string(),
                    },
                    part,
                )
            })?;
            Ok(Value::from(xml))
        }
        RenderValue::Array(items) => {
            let mut converted = Vec::with_capacity(items.len());
            for item in items {
                converted.push(value_to_minijinja(item, registry, shape_id, part)?);
            }
            Ok(Value::from_iter(converted))
        }
        RenderValue::Object(entries) => {
            let mut converted = Vec::with_capacity(entries.len());
            for (key, item) in entries {
                converted.push((
                    Value::from(key.as_str()),
                    value_to_minijinja(item, registry, shape_id, part)?,
                ));
            }
            Ok(Value::from_iter(converted))
        }
    }
}

/// 把图片解析/渲染失败归并为 `TemplateErrorKind::Image`
/// （oracle 异常 `UnrecognizedImageError`）。
fn image_error(err: &ImageResolveError, part: &str) -> RenderError {
    RenderError::Template {
        kind: TemplateErrorKind::Image,
        part: part.to_string(),
        line: None,
        message: err.message.clone(),
        context: Vec::new(),
    }
}

/// 构造与上游一致的 Jinja 环境：lenient undefined（对齐 jinja2 默认
/// `Undefined`），autoescape 仅在显式开启时生效（HTML 规则转义）。
pub(crate) fn build_jinja_env(autoescape: bool) -> Environment<'static> {
    let mut env = Environment::new();
    env.set_fuel(Some(MAX_TEMPLATE_FUEL));
    env.set_undefined_behavior(UndefinedBehavior::Lenient);
    if autoescape {
        env.set_auto_escape_callback(|_| minijinja::AutoEscape::Html);
    }
    env
}

/// 用给定 MiniJinja 根值渲染单个模板字符串；错误映射带 part 名与去标签上下文。
pub(crate) fn render_inline_value(
    env: &Environment,
    template_src: &str,
    root: Value,
    part: &'static str,
) -> Result<String, RenderError> {
    let template = env
        .template_from_str(template_src)
        .map_err(|e| map_jinja_error(&e, template_src, part))?;
    let mut output = LimitedOutput {
        bytes: Vec::new(),
        max: MAX_RENDERED_XML_BYTES,
        exceeded: false,
    };
    let rendered = template.render_captured_to(root, &mut output);
    if output.exceeded {
        return Err(RenderError::Limit {
            part: part.to_string(),
            kind: "rendered_xml_bytes",
            max: MAX_RENDERED_XML_BYTES as u64,
        });
    }
    if let Err(e) = rendered {
        if e.kind() == ErrorKind::OutOfFuel {
            return Err(RenderError::Limit {
                part: part.to_string(),
                kind: "template_fuel",
                max: env.fuel().unwrap_or(MAX_TEMPLATE_FUEL),
            });
        }
        return Err(map_jinja_error(&e, template_src, part));
    }
    String::from_utf8(output.bytes).map_err(|e| RenderError::Template {
        kind: TemplateErrorKind::Other,
        part: part.to_string(),
        line: None,
        message: e.to_string(),
        context: Vec::new(),
    })
}

/// 把 MiniJinja 错误映射为稳定的 [`RenderError`]，附带对齐上游 docx_context
/// 的去标签文本片段（出错行前 4 行起共 7 行）。
fn map_jinja_error(err: &minijinja::Error, prepared: &str, part: &str) -> RenderError {
    let kind = match err.kind() {
        ErrorKind::SyntaxError => TemplateErrorKind::Syntax,
        ErrorKind::UndefinedError => TemplateErrorKind::Undefined,
        _ => TemplateErrorKind::Other,
    };
    let lines: Vec<&str> = prepared.lines().collect();
    let context = if let Some(lineno) = err.line() {
        let start = lineno.saturating_sub(4);
        lines
            .get(start..)
            .unwrap_or(&[])
            .iter()
            .take(7)
            .map(|line| strip_xml_tags().replace_all(line, "").into_owned())
            .collect()
    } else {
        Vec::new()
    };
    RenderError::Template {
        kind,
        part: part.to_string(),
        line: err.line(),
        message: err.to_string(),
        context,
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn output_writer_stops_before_exceeding_limit() {
        let mut output = LimitedOutput {
            bytes: Vec::new(),
            max: 4,
            exceeded: false,
        };
        output.write_all(b"abcd").unwrap();
        assert!(output.write_all(b"e").is_err());
        assert!(output.exceeded);
        assert_eq!(output.bytes, b"abcd");
    }

    #[test]
    fn looping_template_exhausts_fuel() {
        let mut env = build_jinja_env(false);
        env.set_fuel(Some(100));
        let error = render_inline_value(
            &env,
            "{% for i in range(1000) %}x{% endfor %}",
            Value::from_serialize(json!({})),
            MAIN_PART,
        )
        .unwrap_err();
        assert!(
            matches!(
                error,
                RenderError::Limit {
                    kind: "template_fuel",
                    ..
                }
            ),
            "{error:?}"
        );
    }
}

#[cfg(test)]
mod context_render_tests {
    use super::*;
    use crate::context::{ImageRegistry, ImageRels, ImageResolveError};
    use docxtpl_rich::{InlineImage, RichText, RichTextProps};

    /// 固定返回 rId9 的假注册表（记录调用次数验证幂等）。
    struct StubRegistry {
        fail: bool,
        calls: usize,
    }

    impl ImageRegistry for StubRegistry {
        fn resolve_image(&mut self, _image: &InlineImage) -> Result<ImageRels, ImageResolveError> {
            self.calls += 1;
            if self.fail {
                return Err(ImageResolveError {
                    message: "无法识别的图片格式".to_string(),
                });
            }
            Ok(ImageRels {
                blip_rid: "rId9".to_string(),
                hyperlink_rid: None,
            })
        }
    }

    #[test]
    fn shape_id_scans_unprefixed_id_only() {
        assert_eq!(next_shape_id("<w:document/>"), 1);
        // w:id / r:id 带前缀，不计数；docPr id=1 → 2
        let src = r#"<w:document><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:id="99"/></w:numPr></w:pPr>
<w:drawing><wp:inline><wp:docPr id="1" name="Picture 1"/></wp:inline></w:drawing></w:document>"#;
        assert_eq!(next_shape_id(src), 2);
        // 取最大 +1
        let src = r#"<wp:docPr id="1"/><wp:docPr id="5"/><a:hlinkClick r:id="rId9"/>"#;
        assert_eq!(next_shape_id(src), 6);
    }

    #[test]
    fn rich_text_renders_as_markup_inside_run() {
        let mut props = RichTextProps::new();
        props.bold = true;
        let mut ctx = RenderContext::new();
        ctx.insert("rt", RichText::text_with("粗体", &props));

        let src = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{{ rt }}</w:t></w:r></w:p></w:body></w:document>"#;
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let outcome =
            render_document_xml_ctx(src, &ctx, &RenderOptions::compat(), &mut registry).unwrap();
        assert!(outcome.xml.contains("<w:b/>"), "{}", outcome.xml);
        assert!(outcome.xml.contains("粗体"), "{}", outcome.xml);
        // 无图片：注册表不应被调用
        assert_eq!(registry.calls, 0);
    }

    #[test]
    fn inline_image_gets_registry_rids_and_shape_id() {
        let png_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_dot2x1.png"
        );
        let image = InlineImage::from_path(png_path, None, None, None).unwrap();
        let mut ctx = RenderContext::new();
        ctx.insert("img", image);

        let src = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:body><w:p><w:r><w:t>{{ img }}</w:t></w:r></w:p></w:body></w:document>"#;
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let outcome =
            render_document_xml_ctx(src, &ctx, &RenderOptions::compat(), &mut registry).unwrap();
        assert!(outcome.xml.contains(r#"r:embed="rId9""#), "{}", outcome.xml);
        // shape_id=1 经 fix_docpr_ids 后处理重编为 1001 起
        assert!(
            outcome.xml.contains(r#"<wp:docPr id="1001""#),
            "{}",
            outcome.xml
        );
        assert_eq!(registry.calls, 1);
    }

    #[test]
    fn bad_image_maps_to_image_error_kind() {
        let bad_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_bad.png"
        );
        let image = InlineImage::from_path(bad_path, None, None, None).unwrap();
        let mut ctx = RenderContext::new();
        ctx.insert("img", image);

        let src = r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{{ img }}</w:t></w:r></w:p></w:body></w:document>"#;
        // 注册表成功（虚拟 rId），但真实坏字节在 render_inline_image probe 阶段失败
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let error = render_document_xml_ctx(src, &ctx, &RenderOptions::compat(), &mut registry)
            .unwrap_err();
        assert_eq!(error.kind(), Some(TemplateErrorKind::Image));
        assert_eq!(
            error.kind().unwrap().oracle_exception(),
            "UnrecognizedImageError"
        );
    }
}

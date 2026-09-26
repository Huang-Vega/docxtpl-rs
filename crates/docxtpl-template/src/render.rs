//! 渲染主管线：patch → MiniJinja → resolve_listing → recover → fix → serialize。

use docxtpl_compat::{patch_xml, resolve_listing};
use docxtpl_xml::{ns_uri, Recovery, XmlDocument, XmlLimits};
use minijinja::{Environment, ErrorKind, UndefinedBehavior, Value};
use regex::Regex;
use serde_json::Value as JsonValue;
use std::collections::BTreeSet;
use std::io::{self, Write};
use std::sync::OnceLock;

use crate::context::{ImageRegistry, ImageResolveError, NullRegistry, RenderContext, RenderValue};
use crate::error::{RenderError, TemplateErrorKind};
use crate::fix_tables::{fix_docpr_ids, fix_tables};
use docxtpl_rich::InlineImage;

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
    render_part_xml(
        src_xml,
        context,
        options,
        registry,
        MAIN_PART,
        PartKind::Document,
    )
}

/// 页眉/页脚 story part 渲染（P5，ADR-006）：与正文共用
/// patch → jinja → resolve_listing 管线，但**不做 fix_tables /
/// fix_docpr_ids**；shape_id 取自本 part 原始 XML（part 级作用域，
/// docPr 保留本地 id，不像正文被重编为 1001 起）。
///
/// 输出为 lxml 风格完整序列化（单引号 XML 声明），对齐上游把页眉/页脚
/// 重新映射为 XmlPart 后的落盘字节。
pub fn render_story_xml_ctx(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
    part_name: &str,
) -> Result<RenderOutcome, RenderError> {
    render_part_xml(
        src_xml,
        context,
        options,
        registry,
        part_name,
        PartKind::Story,
    )
}

/// 脚注 part 渲染（P5，ADR-006）：patch → jinja → resolve_listing 后
/// **直接返回字符串**，不做 XML 解析/重序列化——上游脚注 part 是通用
/// 二进制 Part，`part._blob = rendered.encode()` 原样保留模板声明与未改
/// 字节。调用方把返回值按 UTF-8 写回原 part。
///
/// 脚注中的 InlineImage 不被支持（上游在通用 Part 上调用
/// new_pic_inline 会 AttributeError，见 DEV-0006）；此入口使用
/// [`NullRegistry`]，出现图片值即报错。
pub fn render_footnotes_xml_ctx(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    part_name: &str,
) -> Result<String, RenderError> {
    let mut null_registry = NullRegistry;
    // 脚注不允许图片，shape_id 不会被消费，传 0 即可；normalize_input=false
    // （通用 Part，原字节直穿，保留模板声明形态）。
    render_part_string(
        src_xml,
        context,
        options,
        &mut null_registry,
        part_name,
        0,
        false,
    )
}

/// 管线处理的 part 种类：决定 shape_id 作用域与渲染后处理（P5，ADR-006）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PartKind {
    /// 正文 word/document.xml：fix_tables + fix_docpr_ids（docPr 1001 起）。
    Document,
    /// 页眉/页脚：只解析愈合后序列化，不动表格与 docPr。
    Story,
}

/// 按 part 种类跑完整管线并返回序列化 XML。
fn render_part_xml(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
    part_name: &str,
    kind: PartKind,
) -> Result<RenderOutcome, RenderError> {
    // shape_id 作用域为单个 part（上游 StoryPart.next_id 对原始树取
    // xpath("//@id") 最大值 +1；正文也在渲染前对原始 document 计算）。
    let shape_id = shape_id_of(src_xml);

    let dst = render_part_string(
        src_xml, context, options, registry, part_name, shape_id, true,
    )?;

    // 宽松(recover)解析愈合（安全限额仍强制）。
    let outcome =
        XmlDocument::parse_lenient(&dst, &XmlLimits::default()).map_err(|e| RenderError::Xml {
            part: part_name.to_string(),
            source: e,
        })?;
    let mut doc = outcome.doc;

    if kind == PartKind::Document {
        // 仅正文执行 fix_tables / fix_docpr_ids（上游 render() 只对 body tree
        // 调这两个后处理）。
        fix_tables(&mut doc)?;
        fix_docpr_ids(&mut doc);
        Ok(RenderOutcome {
            xml: doc.serialize(),
            recoveries: outcome.diagnostics,
        })
    } else {
        // 页眉/页脚映射为新 XmlPart 时按 remove_blank_text 解析（剥除注入
        // 图片 XML 的换行/缩进），且无换挂过程，wp:inline 自带的冗余
        // xmlns:wp/xmlns:r 声明原样保留（ADR-006）。
        doc.strip_blank_text();
        Ok(RenderOutcome {
            xml: doc.serialize_story(),
            recoveries: outcome.diagnostics,
        })
    }
}

/// 管线的字符串阶段：（正文/页眉页脚）树往返归一 → patch → 段落换行 →
/// MiniJinja 渲染 → 还原（换行/字面转义）→ resolve_listing。
///
/// `normalize_input`：正文/页眉页脚为 true（上游 patch 输入来自
/// remove_blank_text lxml 树）；脚注为 false——footnotes part 在
/// python-docx PartFactory 未注册为 XmlPart，是通用二进制 Part，
/// `part.blob` 即磁盘原字节（Word 双引号声明原样穿过 jinja，P7b B5
/// 实证），patch 直接吃原字节、渲染字符串原样写回。
#[allow(clippy::too_many_arguments)]
fn render_part_string(
    src_xml: &str,
    context: &RenderContext,
    options: &RenderOptions,
    registry: &mut dyn ImageRegistry,
    part_name: &str,
    shape_id: i64,
    normalize_input: bool,
) -> Result<String, RenderError> {
    // 0. 正文/页眉页脚：树往返归一（P7b B2）——上游 patch_xml 的输入不是
    //    磁盘原字节：正文是 `tostring(body)`、页眉页脚是
    //    `tostring(parse_xml(part.blob))`，实体解码、缩进剥除、词法归一。
    //    脚注保持原字节（通用 Part blob，声明形态不动）。
    let raw = if normalize_input {
        normalize_part_xml(src_xml, part_name)?
    } else {
        src_xml.to_string()
    };
    // Jinja2 词法器 tnewline=`\r\n|\r|\n` 统一产出 NEWLINE（渲染输出
    // `\n`），即 jinja 往返会吃掉所有 CR（P7b B5：真实 Word 模板脚注
    // 声明后是 `\r\n`，上游输出 `\n`）。lxml 树输出本就无 CR，此替换
    // 只在脚注路径可观测。
    let patch_source = raw.replace("\r\n", "\n").replace('\r', "\n");

    // 1. patch_xml：13 步有界正则变换。
    let patched = patch_xml(&patch_source);

    // 2. 每个段落前插换行（仅用于错误定位，渲染后撤销）。
    let prepared = newline_before_p()
        .replace_all(&patched, "\n<w:p${1}")
        .into_owned();

    // 3. MiniJinja 渲染（默认对齐 jinja2：lenient undefined、autoescape=false）。
    //    图片先以占位符参与渲染（对齐上游惰性 InlineImage.__str__：只解析
    //    本 part 模板实际引用的图片，ADR-006），渲染后再按出现顺序落图。
    let env = build_jinja_env(options.autoescape());
    let (root, pending_images) = context_to_minijinja(context);
    let rendered = render_inline_value(&env, &prepared, root, part_name)?;

    // 4. 撤销换行 + 还原 {_{ }_} 字面转义。
    let dst = newline_before_p_remove()
        .replace_all(&rendered, "<w:p${1}")
        .into_owned();
    let dst = dst
        .replace("{_{", "{{")
        .replace("}_}", "}}")
        // 上游 template.py 的块标签转义是 "{%" 前插 "_" → "{_%"（patch 阶段
        // "{% "→"{_%"），此前缀曾颠倒写成 "{%_"，真实模板的 {_%- 字面文本会
        // 残留进输出（P7b merge_paragraph）。
        .replace("{_%", "{%")
        .replace("%_}", "%}");

    // 5. resolve_listing：\n \t \a \f → br/tab/换段/分页。
    let dst = resolve_listing(&dst);

    // 5.5 图片占位符替换：按输出中的出现顺序解析（rId 由注册表按
    //     reltype/目标/模式去重复用）。shape_id 对同一 part 的所有图片相同：
    //     上游 StoryPart.next_id 每次对**未被渲染改动的原始 part 树**取
    //     max(//@id)+1（无缓存，渲染只产字符串，不回写 part 元素），因此
    //     多张图片拿到同一 id/name；正文再由 fix_docpr_ids 把 id 重排为
    //     1001 起（name 保留 "Picture N"），页眉/页脚则原样保留。
    let dst = substitute_images(&dst, registry, &pending_images, shape_id, part_name)?;
    if dst.len() > MAX_RENDERED_XML_BYTES {
        return Err(RenderError::Limit {
            part: part_name.to_string(),
            kind: "rendered_xml_bytes",
            max: MAX_RENDERED_XML_BYTES as u64,
        });
    }
    Ok(dst)
}

/// XML part 的 python-docx oxml 树形态归一（P7b B1/B2）。
///
/// 两个使用点共用同一形态：
/// 1. 渲染管线 patch_xml 前（B2）——正文/页眉页脚喂给 `patch_xml` 的 XML
///    来自 python-docx oxml 解析树（`remove_blank_text=True`），而非包内
///    磁盘原字节：正文 `tostring(body)`、页眉页脚
///    `tostring(parse_xml(part.blob))`；
/// 2. 保存期已知 XmlPart（styles/settings/numbering）的无条件重序列化
///    （B1）——python-docx 保存时这些 part 恒由 lxml 树重写。
///
/// 脚注 part 不走此函数：它在 PartFactory 未注册为 XmlPart，是通用二进制
/// Part，`blob` 即磁盘原字节（B5 实证 expected 保留 Word 双引号声明）。
///
/// 树往返使实体引用在文本节点中解码为字面字符（jinja 表达式里的
/// `&quot;`/`&apos;` 不再残留）、元素间缩进空白剥除、属性与空元素词法
/// 归一，输出 lxml 风格单引号 XML 声明 + `\n`。模板 part 必为良构 XML，
/// 解析失败按 XML 错误上报（与上游打开文档即失败一致）。
pub fn normalize_part_xml(src_xml: &str, part_name: &str) -> Result<String, RenderError> {
    let mut doc = XmlDocument::parse_strict(src_xml, &XmlLimits::default()).map_err(|source| {
        RenderError::Xml {
            part: part_name.to_string(),
            source,
        }
    })?;
    doc.strip_blank_text();
    Ok(format!(
        "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n{}",
        doc.serialize_subtree(doc.root())
    ))
}

/// 上游 `next_id`：原始 XML 中无前缀 `id="数字"` 属性的最大值 +1（无则 1）。
///
/// 正则要求 `id` 前是空白，因此 `w:id` / `r:id`（冒号紧贴）不会误匹配。
/// 每个渲染 part 独立计算（P5，ADR-006：页眉/页脚 shape_id 为 part 级）。
pub fn shape_id_of(src_xml: &str) -> i64 {
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

/// 图片占位符前缀/后缀（转换期写入，渲染后由 [`substitute_images`] 替换）。
///
/// 只含 `@`、字母、数字，不含 XML/Jinja 特殊字符：autoescape 不转义，
/// patch_xml/resolve_listing 不触碰；用户文本几乎不可能恰好构成该 token。
const IMAGE_TOKEN_PREFIX: &str = "\u{1}DOXTPLRSIMG";
const IMAGE_TOKEN_SUFFIX: &str = "@\u{1}";

/// 图片占位符替换正则（index 为 pending_images 中的下标）。
fn image_token_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(&format!(
            "{}(\\d+){}",
            regex::escape(IMAGE_TOKEN_PREFIX),
            regex::escape(IMAGE_TOKEN_SUFFIX)
        ))
        .expect("invalid regex")
    })
}

/// 把 [`RenderContext`] 转换为 MiniJinja 根值。
///
/// InlineImage 不立即解析，而是替换为占位符（对齐上游惰性
/// `InlineImage.__str__`：只有模板实际引用的图片才在当前 part 的关系作用域
/// 内解析，ADR-006）；返回的 `pending_images` 按下标与占位符对应，
/// [`substitute_images`] 按渲染输出中的出现顺序消费。
pub(crate) fn context_to_minijinja(context: &RenderContext) -> (Value, Vec<&InlineImage>) {
    let mut pending = Vec::new();
    let mut pairs = Vec::with_capacity(context.len());
    for (key, value) in context.iter() {
        pairs.push((Value::from(key), value_to_minijinja(value, &mut pending)));
    }
    (Value::from_iter(pairs), pending)
}

/// 递归把 [`RenderValue`] 转换为 MiniJinja 值。
///
/// RichText/RichTextParagraph/Listing 直接给出生成的 XML 字符串（对齐上游
/// `__str__`，autoescape 缺省关闭）；InlineImage 登记到 `pending` 并以
/// 占位符参与渲染。
fn value_to_minijinja<'a>(value: &'a RenderValue, pending: &mut Vec<&'a InlineImage>) -> Value {
    match value {
        RenderValue::Json(json) => Value::from_serialize(json),
        // 上游四类富值都实现 `__html__`，autoescape 开启时仍作为 Markup
        // 原样注入；MiniJinja 对应使用 safe string。
        RenderValue::RichText(rich) => Value::from_safe_string(rich.to_xml().to_owned()),
        RenderValue::RichTextParagraph(paragraph) => {
            Value::from_safe_string(paragraph.to_xml().to_owned())
        }
        RenderValue::Listing(listing) => Value::from_safe_string(listing.to_xml().to_owned()),
        RenderValue::Image(image) => {
            let index = pending.len();
            pending.push(image);
            Value::from(format!("{IMAGE_TOKEN_PREFIX}{index}{IMAGE_TOKEN_SUFFIX}"))
        }
        // Subdoc 片段按安全字符串注入（P6，ADR-007）：上游 `Subdoc.__html__`
        // 存在，autoescape 开启时 jinja2 走 Markup 不转义；关闭时与
        // `__str__` 等价原样输出。
        RenderValue::Subdoc(fragment) => Value::from_safe_string(fragment.as_str().to_owned()),
        RenderValue::Array(items) => Value::from_iter(
            items
                .iter()
                .map(|item| value_to_minijinja(item, pending))
                .collect::<Vec<_>>(),
        ),
        RenderValue::Object(entries) => Value::from_iter(
            entries
                .iter()
                .map(|(key, item)| (Value::from(key.as_str()), value_to_minijinja(item, pending))),
        ),
    }
}

/// 把渲染输出中的图片占位符替换为真实 `wp:inline`/锚点 XML。
///
/// 按占位符在输出中**出现的顺序**逐次解析（对齐上游 jinja 渲染期惰性
/// `__str__` 的调用顺序）：同一图片出现多次就解析多次——rId 由注册表
/// 按（reltype/目标/模式）去重复用；docPr 的 id/name 全部使用同一个
/// `shape_id`（见 [`shape_id_of`] 与上游无缓存的 `StoryPart.next_id`）。
/// 未被模板引用的图片占位符不会出现，也就不会解析，关系只落在实际引用
/// 它的 part 作用域。
pub(crate) fn substitute_images(
    rendered: &str,
    registry: &mut dyn ImageRegistry,
    pending: &[&InlineImage],
    shape_id: i64,
    part: &str,
) -> Result<String, RenderError> {
    let mut last_error: Option<RenderError> = None;
    let result = image_token_re().replace_all(rendered, |captures: &regex::Captures| {
        if last_error.is_some() {
            // 前一次解析已失败：保持剩余占位符不动（最终整体返回错误）。
            return captures.get(0).unwrap().as_str().to_string();
        }
        let index: usize = match captures[1].parse() {
            Ok(value) => value,
            Err(_) => return captures.get(0).unwrap().as_str().to_string(),
        };
        let Some(image) = pending.get(index).copied() else {
            return captures.get(0).unwrap().as_str().to_string();
        };
        let resolve = (|| {
            let rels = registry
                .resolve_image(image)
                .map_err(|err| image_error(&err, part))?;
            docxtpl_rich::render_inline_image(
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
            })
        })();
        match resolve {
            Ok(xml) => xml,
            Err(err) => {
                last_error = Some(err);
                captures.get(0).unwrap().as_str().to_string()
            }
        }
    });
    match last_error {
        Some(err) => Err(err),
        None => Ok(result.into_owned()),
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
    part: &str,
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

/// 上游 `DocxTemplate.get_undeclared_template_variables`（0.20.2，
/// template.py L894–927）的静态分析：
///
/// 1. 主文档 `w:body` 子树 `xml_to_string` 等价输出后跑 [`patch_xml`]
///    （上游 `self.patch_xml(self.xml_to_string(temp_doc._element.body))`）；
/// 2. 依次追加主 rels 中每个 blob 非空的 header/footer **根元素**
///    （w:hdr / w:ftr）的 patch 输出（上游两遍 rels：先 header 后
///    footer；拼接顺序不影响集合结果）；
/// 3. 裸 jinja 环境 parse 后收集未声明变量——循环变量/宏参数等由
///    jinja 元分析自动排除。
///
/// 返回排序集合以便稳定展示。上游可选的 `context` 差集参数不提供
/// （Rust 侧由调用方对返回集合自行做差集）；不支持自定义 jinja_env。
pub fn find_undeclared_variables(
    doc_xml: &str,
    story_xmls: &[String],
) -> Result<BTreeSet<String>, RenderError> {
    let mut combined = patched_body_xml(doc_xml)?;
    for story in story_xmls {
        combined.push_str(&patched_root_xml(story)?);
    }
    let env = build_jinja_env(false);
    let template = env
        .template_from_str(&combined)
        .map_err(|e| map_jinja_error(&e, &combined, MAIN_PART))?;
    Ok(template.undeclared_variables(false).into_iter().collect())
}

/// 解析主文档，取 w:body 子树序列化并 patch（自省专用，不剥空白）。
fn patched_body_xml(src_xml: &str) -> Result<String, RenderError> {
    let doc = parse_for_introspect(src_xml, MAIN_PART)?;
    // 解析树根节点即顶层 w:document（不存在虚拟 Document 层）。
    let document = doc.root();
    if !is_word_tag(&doc, document, "document") {
        return Err(introspect_structure_error());
    }
    let body = doc
        .children(document)
        .iter()
        .copied()
        .find(|&id| is_word_tag(&doc, id, "body"))
        .ok_or_else(introspect_structure_error)?;
    Ok(patch_xml(&doc.serialize_subtree(body)))
}

/// 解析 header/footer 等 story part，取其根元素（w:hdr / w:ftr）序列化并 patch。
fn patched_root_xml(src_xml: &str) -> Result<String, RenderError> {
    let doc = parse_for_introspect(src_xml, MAIN_PART)?;
    Ok(patch_xml(&doc.serialize_subtree(doc.root())))
}

/// 自省路径的严格解析（良构 docx 必通过；失败按 XML 错误上报）。
fn parse_for_introspect(src_xml: &str, part: &str) -> Result<XmlDocument, RenderError> {
    XmlDocument::parse_strict(src_xml, &XmlLimits::default()).map_err(|source| RenderError::Xml {
        part: part.to_string(),
        source,
    })
}

/// 节点是否为指定本地名的 w: 命名空间元素。
fn is_word_tag(doc: &XmlDocument, id: docxtpl_xml::NodeId, local: &str) -> bool {
    doc.tag(id)
        .is_some_and(|q| q.ns == ns_uri::W && q.local == local)
}

/// 自省输入结构畸形（缺 document/body 等，正常 docx 不可达）。
fn introspect_structure_error() -> RenderError {
    RenderError::Template {
        kind: TemplateErrorKind::Other,
        part: MAIN_PART.to_string(),
        line: None,
        message: "自省失败：文档缺少 w:document/w:body 结构".to_string(),
        context: Vec::new(),
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
        assert_eq!(shape_id_of("<w:document/>"), 1);
        // w:id / r:id 带前缀，不计数；docPr id=1 → 2
        let src = r#"<w:document><w:pPr><w:numPr><w:ilvl w:val="0"/><w:numId w:id="99"/></w:numPr></w:pPr>
<w:drawing><wp:inline><wp:docPr id="1" name="Picture 1"/></wp:inline></w:drawing></w:document>"#;
        assert_eq!(shape_id_of(src), 2);
        // 取最大 +1
        let src = r#"<wp:docPr id="1"/><wp:docPr id="5"/><a:hlinkClick r:id="rId9"/>"#;
        assert_eq!(shape_id_of(src), 6);
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

    /// P5：页眉 story part 渲染后不重编 docPr，新图取本 part 的 next_id
    /// （既有 id=5 → 新图 id=6），既有 docPr 原样保留。
    #[test]
    fn story_part_keeps_local_docpr_ids_without_renumber() {
        let png_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_dot2x1.png"
        );
        let image1 = InlineImage::from_path(png_path, None, None, None).unwrap();
        let image2 = InlineImage::from_path(png_path, None, None, None).unwrap();
        let mut ctx = RenderContext::new();
        ctx.insert("x", "HX");
        ctx.insert("img", image1);
        ctx.insert("img2", image2);

        let src = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:wp="http://schemas.openxmlformats.org/drawingml/2006/wordprocessingDrawing" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><w:p><w:r><w:t>{{x}}{{img}}{{img2}}</w:t></w:r></w:p><w:p><w:r><w:drawing><wp:inline><wp:docPr id="5" name="Existing"/></wp:inline></w:drawing></w:r></w:p></w:hdr>"#;
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let outcome = render_story_xml_ctx(
            src,
            &ctx,
            &RenderOptions::compat(),
            &mut registry,
            "word/header1.xml",
        )
        .unwrap();
        // 既有 docPr id=5 保留（正文模式会被 fix_docpr_ids 改成 1003）
        assert!(
            outcome.xml.contains(r#"<wp:docPr id="5""#),
            "{}",
            outcome.xml
        );
        // 两张新图 shape_id 相同（上游 next_id 对原始树无缓存重复求值，
        // 均为 max(5)+1=6），正文 fix_docpr_ids 不作用于 story part。
        assert_eq!(
            outcome.xml.matches(r#"<wp:docPr id="6""#).count(),
            2,
            "{}",
            outcome.xml
        );
        assert!(outcome.xml.contains("HX"), "{}", outcome.xml);
        assert_eq!(registry.calls, 2);
    }

    /// P5：页眉语法错误带上具体 part 名，类别为 Syntax。
    #[test]
    fn story_syntax_error_carries_header_part_name() {
        let ctx = RenderContext::new();
        let src = r#"<w:hdr xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:p><w:r><w:t>{% if x %}open</w:t></w:r></w:p></w:hdr>"#;
        let mut registry = StubRegistry {
            fail: false,
            calls: 0,
        };
        let error = render_story_xml_ctx(
            src,
            &ctx,
            &RenderOptions::compat(),
            &mut registry,
            "word/header1.xml",
        )
        .unwrap_err();
        assert_eq!(error.kind(), Some(TemplateErrorKind::Syntax));
        assert!(error.to_string().contains("word/header1.xml"), "{error}");
    }

    /// P5：脚注输出为原始字符串——模板声明/格式保留，仅做 jinja 替换与
    /// resolve_listing，不经 XML 解析重序列化。
    #[test]
    fn footnotes_render_keeps_raw_declaration_and_bytes() {
        let mut ctx = RenderContext::new();
        ctx.insert("z", "ZZ");
        let src = "<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n".to_string()
            + r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1"><w:p><w:r><w:t>FN {{z}}</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
        let out =
            render_footnotes_xml_ctx(&src, &ctx, &RenderOptions::compat(), "word/footnotes.xml")
                .unwrap();
        // 声明原样保留（story/document 模式会统一重写声明）
        assert!(
            out.starts_with("<?xml version='1.0' encoding='UTF-8' standalone='yes'?>\n"),
            "{out}"
        );
        assert!(out.contains("FN ZZ"), "{out}");
    }

    /// P5：脚注里出现 InlineImage → NullRegistry 拒绝（DEV-0006）。
    #[test]
    fn footnotes_reject_inline_image() {
        let png_path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_dot2x1.png"
        );
        let image = InlineImage::from_path(png_path, None, None, None).unwrap();
        let mut ctx = RenderContext::new();
        ctx.insert("img", image);
        let src = r#"<w:footnotes xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:footnote w:id="1"><w:p><w:r><w:t>{{img}}</w:t></w:r></w:p></w:footnote></w:footnotes>"#;
        assert!(render_footnotes_xml_ctx(
            src,
            &ctx,
            &RenderOptions::compat(),
            "word/footnotes.xml"
        )
        .is_err());
    }
}

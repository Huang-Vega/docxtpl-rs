//! Python oracle 端到端差分（规划文档 §9，代码规范 §5.2）。
//!
//! 仅在 `--features oracle` 下编译运行：对 manifest 中每个 mode=render 的
//! fixture，用 Rust 门面渲染，再调用 `tests/oracle/compare.py` 与固定版本
//! Python docxtpl 0.20.2 的输出做语义（c14n + part/rels/content-types）比较。
//!
//! - `context_kind="json"`（或缺省）：从 manifest 指向的 JSON 文件加载上下文；
//! - `context_kind="python"`（p4_*）：上下文在本测试中用 Rust 富内容类型
//!   1:1 复刻 `tests/fixtures/contexts/<id>.py` 的 `build_context(tpl)`
//!   （含 tpl.build_url_id 预登记外链），不执行 Python。
//!
//! error 预期 fixture：比较错误类别（oracle error_type vs Rust
//! TemplateErrorKind::oracle_exception）。

#![cfg(feature = "oracle")]

use std::path::{Path, PathBuf};
use std::process::Command;

use docxtpl_rs::{
    DocxTemplate, InlineImage, Listing, RenderContext, RenderOptions, RenderSession, RenderValue,
    RichText, RichTextParagraph, RichTextProps, TemplateErrorKind,
};
use serde_json::Value;

fn fixtures_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/docxtpl-rs
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("fixtures")
}

fn oracle_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR = <repo>/crates/docxtpl-rs
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("tests")
        .join("oracle")
}

fn python() -> String {
    std::env::var("DOCXTPL_PYTHON").unwrap_or_else(|_| "python".to_string())
}

fn load_json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("读取 {} 失败: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("解析 {} 失败: {e}", path.display()))
}

/// 1 mm 对应的 EMU（python-docx `Mm`：914400/25.4 = 36000）。
const EMU_PER_MM: i64 = 36_000;

/// 构造 RichTextProps 的测试辅助（对齐 Python 关键字参数）。
fn props(build: impl FnOnce(&mut RichTextProps)) -> RichTextProps {
    let mut p = RichTextProps::new();
    build(&mut p);
    p
}

/// 从 tests/fixtures/media 读取图片构造 InlineImage
/// （对齐 contexts/*.py 的 `InlineImage(tpl, _img(name), ...)`）。
fn media_image(
    root: &Path,
    name: &str,
    width: Option<i64>,
    height: Option<i64>,
    anchor: Option<&str>,
) -> InlineImage {
    let path = root.join("media").join(name);
    InlineImage::from_path(
        path.to_str().expect("媒体路径合法 UTF-8"),
        width,
        height,
        anchor.map(str::to_string),
    )
    .unwrap_or_else(|e| panic!("读取图片 {} 失败: {e}", path.display()))
}

/// 读取 tests/fixtures/media/<name> 的原始字节（P7 替换素材：源/目标字节）。
fn media_bytes(root: &Path, name: &str) -> Vec<u8> {
    let path = root.join("media").join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("读取素材 {} 失败: {e}", path.display()))
}

/// 1:1 复刻 contexts/<id>.py 的 build_context(tpl)。
///
/// p4 起的 context_kind="python" fixture 均在此复刻；新增 fixture 时必须
/// 在此同步添加分支（未知 id 直接 panic，防止漏构造）。`tpl` 供 P7
/// 模板自省（undeclared_variables）等 DocxTemplate 级调用使用。
fn build_python_context(
    id: &str,
    root: &Path,
    tpl: &DocxTemplate,
    session: &mut RenderSession,
) -> RenderContext {
    let mut ctx = RenderContext::new();
    match id {
        "p4_rt_basic" => {
            // RichText("中文<b>粗", bold=True, color="#FF0000", size="20")
            // 再 add 斜体/下划线(strikethrough? no: underline=True)/删除线/高亮
            let mut rt = RichText::text_with(
                "中文<b>粗",
                &props(|p| {
                    p.bold = true;
                    p.color = Some("#FF0000".to_owned());
                    p.size = Some("20".to_owned());
                }),
            );
            rt.add_with("斜体", &props(|p| p.italic = true));
            // 上游 underline=True（非合法字符串）归一为 "single"
            rt.add_with(
                "下划线",
                &props(|p| p.underline = Some("single".to_owned())),
            );
            rt.add_with("删除线", &props(|p| p.strike = true));
            rt.add_with("高亮", &props(|p| p.highlight = Some("#FFFF00".to_owned())));
            ctx.insert("rt", rt);
            ctx.insert("empty", RichText::new());
        }
        "p4_rt_style_font" => {
            let mut rt1 =
                RichText::text_with("样式", &props(|p| p.style = Some("Emphasis".to_owned())));
            rt1.add_with(
                "区域字体",
                &props(|p| p.font = Some("eastAsia:SimSun".to_owned())),
            );
            let rt2 = RichText::text_with(
                "上标",
                &props(|p| {
                    p.superscript = true;
                    p.lang = Some("zh-CN".to_owned());
                }),
            );
            let mut rt3 = RichText::text_with(
                "RTL",
                &props(|p| {
                    p.bold = true;
                    p.rtl = true;
                }),
            );
            rt3.add_with("下标", &props(|p| p.subscript = true));
            ctx.insert("rt1", rt1);
            ctx.insert("rt2", rt2);
            ctx.insert("rt3", rt3);
        }
        "p4_rt_url" => {
            // url_id = tpl.build_url_id("https://docxtpl.readthedocs.io/")
            let url_id = session.build_url_id("https://docxtpl.readthedocs.io/");
            let rt = RichText::text_with(
                "链接文本",
                &props(|p| {
                    p.url_id = Some(url_id);
                    p.color = Some("0563C1".to_owned());
                    p.underline = Some("single".to_owned());
                }),
            );
            ctx.insert("rt", rt);
        }
        "p4_rt_in_table" => {
            let rt = RichText::text_with(
                "单元格",
                &props(|p| {
                    p.bold = true;
                    p.size = Some("18".to_owned());
                }),
            );
            ctx.insert("rt", rt);
        }
        "p4_rtp_basic" => {
            // RichTextParagraph("首段", parastyle="ListBullet")
            // rp.add(RichText("次段粗体", bold=True), parastyle=None)
            let mut rp = RichTextParagraph::with_text("首段", "ListBullet");
            let bold = RichText::text_with("次段粗体", &props(|p| p.bold = true));
            rp.add_rich(&bold, "");
            ctx.insert("rp", rp);
        }
        "p4_listing_basic" => {
            ctx.insert("lst", Listing::new("L1\nL2\tT3\u{7}P4\u{c}PAGE"));
        }
        "p4_listing_after_rt" => {
            ctx.insert("rt", RichText::text("前"));
            ctx.insert("lst", Listing::new("X\nY"));
        }
        "p4_img_png" => {
            ctx.insert("img", media_image(root, "p4_dot2x1.png", None, None, None));
        }
        "p4_img_scale_w" => {
            // width=Mm(20)
            ctx.insert(
                "img",
                media_image(root, "p4_wide4x1.png", Some(20 * EMU_PER_MM), None, None),
            );
        }
        "p4_img_wh" => {
            // width=Mm(10), height=Mm(3)
            ctx.insert(
                "img",
                media_image(
                    root,
                    "p4_rect4x2.jpg",
                    Some(10 * EMU_PER_MM),
                    Some(3 * EMU_PER_MM),
                    None,
                ),
            );
        }
        "p4_img_dup" => {
            // 两个独立 InlineImage 对象、字节相同：sha1 去重复用同一 part/rId
            ctx.insert("i1", media_image(root, "p4_dot2x1.png", None, None, None));
            ctx.insert("i2", media_image(root, "p4_dot2x1.png", None, None, None));
        }
        "p4_img_two" => {
            ctx.insert("i1", media_image(root, "p4_dot2x1.png", None, None, None));
            ctx.insert("i2", media_image(root, "p4_rect4x2.jpg", None, None, None));
        }
        "p4_img_anchor" => {
            ctx.insert(
                "img",
                media_image(
                    root,
                    "p4_dot2x1.png",
                    None,
                    None,
                    Some("https://example.com/"),
                ),
            );
        }
        "p4_img_formats" => {
            ctx.insert(
                "bmp",
                media_image(root, "p4_brick4x2.bmp", None, None, None),
            );
            ctx.insert(
                "gif",
                media_image(root, "p4_arrow4x2.gif", None, None, None),
            );
            ctx.insert(
                "tif",
                media_image(root, "p4_tile4x2.tiff", None, None, None),
            );
        }
        "p4_img_in_table" => {
            // 同一个 InlineImage 值在两行中复用（渲染期解析两次，幂等）
            let img = media_image(root, "p4_dot2x1.png", None, None, None);
            let rows = RenderValue::array(vec![
                RenderValue::object(vec![
                    ("n".to_owned(), "r1".into()),
                    ("img".to_owned(), img.clone().into()),
                ]),
                RenderValue::object(vec![
                    ("n".to_owned(), "r2".into()),
                    ("img".to_owned(), img.into()),
                ]),
            ]);
            ctx.insert("rows", rows);
        }
        "p4_img_bad" => {
            // 文件可读但字节不是受支持图片：渲染期 probe 报 UnrecognizedImageError
            ctx.insert("img", media_image(root, "p4_bad.png", None, None, None));
        }
        "p4_combo_rich" => {
            let title = RichText::text_with(
                "INV-2026-001",
                &props(|p| {
                    p.bold = true;
                    p.color = Some("1F4E79".to_owned());
                    p.size = Some("24".to_owned());
                }),
            );
            let img1 = media_image(root, "p4_dot2x1.png", None, None, None);
            let img2 = media_image(root, "p4_dot2x1.png", None, None, None);
            let rows = RenderValue::array(vec![
                RenderValue::object(vec![
                    (
                        "name".to_owned(),
                        RichText::text_with("Alpha", &props(|p| p.bold = true)).into(),
                    ),
                    ("qty".to_owned(), 1_i64.into()),
                    ("img".to_owned(), img1.into()),
                ]),
                RenderValue::object(vec![
                    (
                        "name".to_owned(),
                        RichText::text_with("Beta", &props(|p| p.italic = true)).into(),
                    ),
                    ("qty".to_owned(), 2_i64.into()),
                    ("img".to_owned(), img2.into()),
                ]),
            ]);
            ctx.insert("title", title);
            ctx.insert("rows", rows);
            ctx.insert("notes", Listing::new("first line\nsecond line\tlast"));
        }
        "p5_footnotes_basic" => {
            // footnotes part 走通用二进制 part 路径（原始字符串写回）
            ctx.insert("bn", "正文引用");
            ctx.insert("fn", "脚注值");
            ctx.insert(
                "rt",
                RichText::text_with("富文本", &props(|p| p.bold = true)),
            );
            ctx.insert("lst", Listing::new("L1\nL2\tT2"));
        }
        "p5_hf_image" => {
            // 同字节图片在正文/页眉/页脚：包级 media 去重，三套独立 rels。
            // 插入序与各 part 模板引用序一致（惰性解析，关系只落在引用它的 part）。
            ctx.insert("ht", "页眉");
            ctx.insert("himg", media_image(root, "p4_dot2x1.png", None, None, None));
            ctx.insert(
                "himg2",
                media_image(
                    root,
                    "p4_dot2x1.png",
                    None,
                    None,
                    Some("https://example.com/anchor"),
                ),
            );
            ctx.insert("ft", "页脚");
            ctx.insert("fimg", media_image(root, "p4_dot2x1.png", None, None, None));
            ctx.insert("bt", "正文");
            ctx.insert("bimg", media_image(root, "p4_dot2x1.png", None, None, None));
        }
        "p5_hf_richtext" => {
            // url_id 恒登记在主文档 rels（build_url_id），正文 RichText 引用
            let url_id = session.build_url_id("https://docxtpl.readthedocs.io/p5");
            ctx.insert(
                "hrt",
                RichText::text_with(
                    "页眉富",
                    &props(|p| {
                        p.bold = true;
                        p.color = Some("1F4E79".to_owned());
                    }),
                ),
            );
            ctx.insert("hlst", Listing::new("HL1\nHL2"));
            ctx.insert(
                "frt",
                RichText::text_with("页脚富", &props(|p| p.italic = true)),
            );
            ctx.insert(
                "brt",
                RichText::text_with(
                    "正文链接",
                    &props(|p| {
                        p.url_id = Some(url_id);
                        p.underline = Some("single".to_owned());
                        p.color = Some("0563C1".to_owned());
                    }),
                ),
            );
        }
        "p6_subdoc_basic" | "p6_subdoc_style" | "p6_subdoc_image" | "p6_subdoc_verbatim"
        | "p6_subdoc_untagged" => {
            // 1:1 复刻 contexts/p6_*.py：sd = tpl.new_subdoc(<id>_sub.docx)
            // （上游在 build_context 构造期执行部件合并，此处同样先于 finish）。
            let sub_path = root.join("templates").join(format!("{id}_sub.docx"));
            let sd = session
                .new_subdoc(&sub_path)
                .unwrap_or_else(|e| panic!("{id}: new_subdoc 合并失败: {e}"));
            ctx.insert("sd", sd);
        }

        // ---------- P7 媒体/嵌入替换族（ADR-008）----------
        "p7_media_body" => {
            // 1:1 复刻 contexts/p7_media_body.py 的 tpl.replace_media(...)。
            session.replace_media(
                media_bytes(root, "p7_dummy.png"),
                media_bytes(root, "p7_media3x2.png"),
            );
            ctx.insert("x", "正文媒体替换");
        }
        "p7_media_header" => {
            session.replace_media(
                media_bytes(root, "p7_dummy.png"),
                media_bytes(root, "p7_media3x2.png"),
            );
            ctx.insert("y", "页眉媒体替换");
        }
        "p7_pic_match" => {
            // 注册序即上游 dict 插入序：image.png 命中第一张、
            // my-title 命中第二张（第二张 cNvPr name 已改名避免遮蔽）。
            session.replace_pic("image.png", media_bytes(root, "p7_media3x2.png"));
            session.replace_pic("my-title", media_bytes(root, "p7_new4x4.png"));
            ctx.insert("z", "图片标识替换");
        }
        "p7_pic_missing" => {
            // 模板无 nope.png 标识 → finish 的 pre_processing 报 ValueError。
            session.replace_pic("nope.png", media_bytes(root, "p7_media3x2.png"));
            ctx.insert("w", "缺失标识");
        }
        "p7_embedded_zipname" => {
            // ole1 走 CRC（word/embeddings/ 前缀），ole2 走 zipname 精确命中。
            session.replace_embedded(
                media_bytes(root, "p7_embed_orig1.bin"),
                media_bytes(root, "p7_embed_new1.bin"),
            );
            session.replace_zipname(
                "word/embeddings/p7_ole2.bin",
                media_bytes(root, "p7_embed_new2.bin"),
            );
            ctx.insert("e", "嵌入替换");
        }
        "p7_replace_only" => {
            // 不渲染直接保存（skip_render）：仅注册 CRC 媒体替换。
            session.replace_media(
                media_bytes(root, "p7_dummy.png"),
                media_bytes(root, "p7_media3x2.png"),
            );
        }
        "p7_undeclared_vars" => {
            // 对齐 ctx：vars = ",".join(sorted(get_undeclared_template_variables()))
            let names = tpl
                .undeclared_variables()
                .unwrap_or_else(|e| panic!("{id}: undeclared_variables 失败: {e}"));
            ctx.insert("vars", names.into_iter().collect::<Vec<_>>().join(","));
        }

        // ---------- P7b 真实 Word 手工模板（1:1 复刻 contexts/p7b_*.py）----------
        "p7b_word2016" => {
            // 纯空格/制表符：str 与 RichText 各一对（xml:space preserve 实测）。
            ctx.insert("test_space", "          ");
            ctx.insert("test_tabs", "\t".repeat(5));
            ctx.insert("test_space_r", RichText::text("          "));
            ctx.insert("test_tabs_r", RichText::text(&"\t".repeat(5)));
        }
        "p7b_cellbg" => {
            // 4 行告警：第 1 行 RichText 红字加粗；bg 是单元格背景变量。
            let alerts = RenderValue::array(vec![
                RenderValue::object(vec![
                    ("date".to_owned(), "2015-03-10".into()),
                    (
                        "desc".to_owned(),
                        RichText::text_with(
                            "Very critical alert",
                            &props(|p| {
                                p.color = Some("FF0000".to_owned());
                                p.bold = true;
                            }),
                        )
                        .into(),
                    ),
                    ("type".to_owned(), "CRITICAL".into()),
                    ("bg".to_owned(), "FF0000".into()),
                ]),
                RenderValue::object(vec![
                    ("date".to_owned(), "2015-03-11".into()),
                    ("desc".to_owned(), RichText::text("Just a warning").into()),
                    ("type".to_owned(), "WARNING".into()),
                    ("bg".to_owned(), "FFDD00".into()),
                ]),
                RenderValue::object(vec![
                    ("date".to_owned(), "2015-03-12".into()),
                    ("desc".to_owned(), RichText::text("Information").into()),
                    ("type".to_owned(), "INFO".into()),
                    ("bg".to_owned(), "8888FF".into()),
                ]),
                RenderValue::object(vec![
                    ("date".to_owned(), "2015-03-13".into()),
                    ("desc".to_owned(), RichText::text("Debug trace").into()),
                    ("type".to_owned(), "DEBUG".into()),
                    ("bg".to_owned(), "FF00FF".into()),
                ]),
            ]);
            ctx.insert("alerts", alerts);
        }
        "p7b_richtext_if" => {
            ctx.insert(
                "foobar",
                RichText::text_with("Foobar!", &props(|p| p.color = Some("ff0000".to_owned()))),
            );
        }
        "p7b_eastasia" => {
            // eastAsia: 前缀 → rFonts w:eastAsia（三种字体名写法）。
            ctx.insert(
                "example",
                RichText::text_with(
                    "测试TEST",
                    &props(|p| p.font = Some("eastAsia:Microsoft YaHei".to_owned())),
                ),
            );
            ctx.insert(
                "Chinese",
                RichText::text_with(
                    "测试TEST",
                    &props(|p| p.font = Some("eastAsia:微软雅黑".to_owned())),
                ),
            );
            ctx.insert(
                "simsun",
                RichText::text_with(
                    "测试TEST",
                    &props(|p| p.font = Some("eastAsia:SimSun".to_owned())),
                ),
            );
        }
        other => panic!("未实现的 python 上下文 fixture: {other}"),
    }
    ctx
}

/// 调用 compare.py，返回 (退出成功?, stdout)。
fn run_compare(expected: &Path, actual: &Path) -> (bool, String) {
    let script = oracle_dir().join("compare.py");
    let output = Command::new(python())
        .arg(&script)
        .arg(expected)
        .arg(actual)
        .output()
        .expect("无法启动 python；请安装 Python 或用 DOCXTPL_PYTHON 指定解释器");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let ok = output.status.success() || stdout.trim() == "MATCH";
    (ok, stdout + &String::from_utf8_lossy(&output.stderr))
}

#[test]
fn oracle_differential_render_fixtures() {
    let root = fixtures_root();
    let manifest = load_json(&root.join("manifest.json"));
    let report = load_json(&oracle_dir().join("expected").join("report.json"));

    let fixtures = manifest["fixtures"]
        .as_array()
        .expect("manifest.fixtures 非数组");
    let render_ids: Vec<&str> = fixtures
        .iter()
        .filter(|fx| fx["mode"] == "render")
        .map(|fx| fx["id"].as_str().unwrap())
        .collect();
    // P0–P3 49 + P4 17 + P5 7 + P6 5 + P7 7 + P7b 16 = 101 个 render fixture。
    assert_eq!(
        render_ids.len(),
        101,
        "render fixture 计数应为 101（实际 {}）",
        render_ids.len()
    );

    let mut matched = 0usize;
    let mut error_matched = 0usize;
    let mut failures = Vec::new();

    for id in render_ids {
        let fx = fixtures
            .iter()
            .find(|f| f["id"] == id)
            .expect("manifest 条目存在");
        let template_path = root.join(
            fx["template"]
                .as_str()
                .unwrap()
                .replace('/', std::path::MAIN_SEPARATOR_STR),
        );

        let expected_error_type = report["fixtures"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == id)
            .and_then(|f| f["error_type"].as_str())
            .map(str::to_string);

        let is_python_context = fx
            .get("context_kind")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind == "python");

        let tpl = DocxTemplate::open(&template_path)
            .unwrap_or_else(|e| panic!("{id}: 打开模板失败: {e}"));

        // P7：skip_render=true 对齐上游不 render 直接 save（仍先
        // build_context 以注册 replace_*，随后走 finish_without_render）。
        let skip_render = fx
            .get("skip_render")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        // python 上下文走富内容渲染会话（build_url_id 在会话内预登记）；
        // 其余走纯 JSON 路径。
        let rendered = if is_python_context {
            let mut session = tpl
                .render_session(&RenderOptions::compat())
                .unwrap_or_else(|e| panic!("{id}: 开启渲染会话失败: {e}"));
            let ctx = build_python_context(id, &root, &tpl, &mut session);
            if skip_render {
                session.finish_without_render()
            } else {
                session.finish(&ctx)
            }
        } else {
            let ctx = match fx.get("context").and_then(Value::as_str) {
                Some(rel) => load_json(&root.join(rel)),
                None => Value::Object(serde_json::Map::new()),
            };
            tpl.render(&ctx, &RenderOptions::compat())
        };

        if let Some(expected_type) = expected_error_type {
            // error 预期：只比较稳定错误类别。
            match rendered {
                Err(err) => {
                    let actual = err
                        .kind()
                        .map(TemplateErrorKind::oracle_exception)
                        .unwrap_or("(xml/io 错误)");
                    if actual == expected_type {
                        error_matched += 1;
                    } else {
                        failures.push(format!(
                            "{id}: 错误类别不一致 oracle={expected_type} rust={actual} msg={err}"
                        ));
                    }
                }
                Ok(_) => failures.push(format!(
                    "{id}: oracle 报错 {expected_type}，Rust 却渲染成功"
                )),
            }
            continue;
        }

        let doc = match rendered {
            Ok(doc) => doc,
            Err(e) => {
                failures.push(format!("{id}: Rust 渲染意外失败: {e:?}"));
                continue;
            }
        };

        let tmp = std::env::temp_dir().join(format!("docxtplrs_oracle_{id}.docx"));
        doc.save(&tmp)
            .unwrap_or_else(|e| panic!("{id}: 写出临时文件失败: {e}"));
        let expected = oracle_dir().join("expected").join(format!("{id}.docx"));
        let (ok, detail) = run_compare(&expected, &tmp);
        let _ = std::fs::remove_file(&tmp);
        if ok {
            matched += 1;
        } else {
            failures.push(format!("{id}: 差分不一致\n{detail}"));
        }
    }

    eprintln!(
        "oracle 差分：成功语义匹配 {matched}，错误类别匹配 {error_matched}，失败 {}",
        failures.len()
    );
    assert!(
        failures.is_empty(),
        "共 {} 个 fixture 与 oracle 不一致:\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}

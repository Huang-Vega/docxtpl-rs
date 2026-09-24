//! Python oracle 端到端差分（规划文档 §9，代码规范 §5.2）。
//!
//! 仅在 `--features oracle` 下编译运行：对 manifest 中每个 mode=render 的
//! fixture，用 Rust 门面渲染，再调用 `tests/oracle/compare.py` 与固定版本
//! Python docxtpl 0.20.2 的输出做语义（c14n + part/rels/content-types）比较。
//!
//! error 预期 fixture：比较错误类别（oracle error_type vs Rust
//! TemplateErrorKind::oracle_exception）。

#![cfg(feature = "oracle")]

use std::path::{Path, PathBuf};
use std::process::Command;

use docxtpl_rs::{DocxTemplate, RenderOptions, TemplateErrorKind};
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
    assert!(
        render_ids.len() >= 48,
        "render fixture 不足 48 个（实际 {}）",
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

        let ctx = match fx.get("context").and_then(Value::as_str) {
            Some(rel) => load_json(&root.join(rel)),
            None => Value::Object(serde_json::Map::new()),
        };

        let tpl = DocxTemplate::open(&template_path)
            .unwrap_or_else(|e| panic!("{id}: 打开模板失败: {e}"));
        let rendered = tpl.render(&ctx, &RenderOptions::compat());

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

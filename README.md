# docxtpl-rs

将 DOCX 主文档中的 Jinja 模板渲染为 Word 文档的 Rust 库和命令行工具。
当前版本为 `0.8.0`，对照 Python docxtpl 0.20.2 的 P0～P7 冻结功能集。

## 使用

需要 Rust 1.85 或更新版本。克隆仓库后运行：

```sh
cargo build -p docxtpl-cli
cargo run -p docxtpl-cli -- render template.docx context.json output.docx
```

`context.json` 是 JSON 对象，例如 `{"name":"Vega","items":[{"name":"Apple"}]}`。
库调用方式见 [docxtpl-rs 示例](crates/docxtpl-rs/src/lib.rs)。

## 已支持的范围

普通变量、控制结构、高级表格、RichText/Listing/InlineImage、页眉页脚与
脚注字符串渲染、外部 Subdoc 合并、媒体/嵌入替换及模板变量自省。
精确用例、已知偏差和 oracle 结果见 [兼容清单](docs/compatibility.md)。

未支持边界（任意 Python 对象、自定义 Jinja2 扩展、部分 Subdoc 长尾等）
均在兼容清单中以 `DEV-*` 公布。
默认输入及渲染预算见 [资源限额](docs/security-limits.md)。

## 验证

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
python -m pip install -r tests/oracle/requirements.txt
cargo test -p docxtpl-rs --features oracle --test oracle_diff
cargo build -p docxtpl-cli
python tests/office_check.py
```

最后一步需安装 LibreOffice。P7 验收结论见
[P7 兼容报告](docs/p7-compatibility-report.md)。

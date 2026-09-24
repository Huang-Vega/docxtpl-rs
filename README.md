# docxtpl-rs

将 DOCX 主文档中的 Jinja 模板渲染为 Word 文档的 Rust 库和命令行工具。
当前版本为 `0.1.0-alpha`，对照 Python docxtpl 0.20.2 的 P0～P3 功能集。

## 使用

需要 Rust 1.85 或更新版本。克隆仓库后运行：

```sh
cargo build -p docxtpl-cli
cargo run -p docxtpl-cli -- render template.docx context.json output.docx
```

`context.json` 是 JSON 对象，例如 `{"name":"Vega","items":[{"name":"Apple"}]}`。
库调用方式见 [docxtpl-rs 示例](crates/docxtpl-rs/src/lib.rs)。

## 已支持的范围

主文档中的普通变量、常见过滤器、if/for、空白控制、转义、跨 run 标签，
以及 p/tr/tc/r 结构标签、表格行与单元格循环、colspan/cellbg/vm/hm。
精确用例、已知偏差和 oracle 结果见 [兼容清单](docs/compatibility.md)。

当前不渲染页眉页脚、脚注、图片、RichText、subdoc 等 P4～P6 功能。
默认输入及渲染预算见 [资源限额](docs/security-limits.md)。

## 验证

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
python -m pip install -r tests/oracle/requirements.txt
cargo test -p docxtpl-rs --features oracle --test oracle_diff
cargo build -p docxtpl-cli
python tests/office_check.py
```

最后一步需安装 LibreOffice。当前验收记录见 [P0～P3 验收报告](docs/acceptance-P0-P3.md)。

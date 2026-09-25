# P0～P3 验收记录

审查基线：`298803b`；修复提交：`bd9aadf`。
日期：2026-09-25。

| 阶段 | 已验证 | 待完成 |
|---|---|---|
| P0 | 上游 0.20.2 指纹；69 个有阶段标记的 fixture；oracle runner 和差分 | 无 |
| P1 | 20 个 OPC 往返测试；69 个模板的资源指标已记录在 security-limits.md | 无 |
| P2 | 本机 `cargo test --workspace` 和 49 个 oracle 渲染用例通过；LibreOffice 打开并另存基础变量文档；WPS 打开、编辑并保存发票样例 | 无 |
| P3 | LibreOffice 打开并另存 7 个表格用例；表格、行和单元格数量符合预期；WPS 检查纵向合并单元格的显示 | alpha tag/release |

本机验证：macOS arm64，Rust stable 1.98.1；`cargo fmt`、Clippy、
workspace tests、Rust 1.85 `cargo check --workspace --locked`、Python oracle
差分均通过。LibreOfficeDev 26.8.0.0.alpha0 的 8 个打开/另存检查通过。

修复提交的 [CI 运行](https://github.com/Huang-Vega/docxtpl-rs/actions/runs/36036764941)
中，Linux、macOS、Windows、oracle（含 LibreOffice）和 MSRV 全部通过。
WPS Office for macOS 12.1.26046 人工验收：`r3_combo_invoice.docx` 正常显示
发票标题、3 列表格及两条数据；将首条数据 `A` 改为 `A TEST` 后保存，
重新读取文档确认文本与 3×3 表格结构保留。`r3_vm.docx` 正常显示
纵向合并的 `VX` 单元格及相邻的 `a`、`b`、`c`。WPS 提示缺失字体，
因此本次外观检查确认内容与布局结构，不确认字体完全一致。

按实际使用环境以 WPS 和 LibreOffice 完成 Office 兼容验收。本机未安装
Microsoft Word，Word 专属兼容性尚未直接验证。`0.1.0-alpha` 尚未发布。

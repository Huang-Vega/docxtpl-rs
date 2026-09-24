# P0～P3 验收记录

审查基线：`298803b`；修复后的代码在本地工作区，待 CI 重跑。
日期：2026-09-25。

| 阶段 | 已验证 | 待完成 |
|---|---|---|
| P0 | 上游 0.20.2 指纹；69 个有阶段标记的 fixture；oracle runner 和差分 | 无 |
| P1 | 20 个 OPC 往返测试；69 个模板的资源指标已记录在 security-limits.md | 修复提交的 CI 重跑 |
| P2 | 本机 `cargo test --workspace` 和 49 个 oracle 渲染用例通过；LibreOffice 打开并另存基础变量文档 | Word 人工打开、编辑、外观检查 |
| P3 | LibreOffice 打开并另存 7 个表格用例；表格、行和单元格数量符合预期 | Word 人工检查表格编辑与外观；alpha tag/release |

本机验证：macOS arm64，Rust stable 1.98.1；`cargo fmt`、Clippy、
workspace tests、Rust 1.85 `cargo check --workspace --locked`、Python oracle
差分均通过。LibreOfficeDev 26.8.0.0.alpha0 的 8 个打开/另存检查通过。

仍须在修复提交上等待 Linux、macOS、Windows、oracle 和 MSRV 的 CI 全绿。
本机未安装 Microsoft Word；Word 人工验收和 `0.1.0-alpha` 发布在这两项完成后执行。

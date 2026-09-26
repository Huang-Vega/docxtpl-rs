# macOS 实机验证（0.8.0 候选）

日期：2026-09-26。基线：`fcfceec`，实测包含下述 XML panic 修复。
本机：macOS 26.6.2（25G83）、arm64、rustc 1.98.1、Python 3.14.4。
Python 依赖按 `tests/oracle/requirements.txt` 安装。

## 验证结果

| 检查 | 命令 | 结果 |
|---|---|---|
| 格式 | `cargo fmt --all -- --check` | 通过 |
| Clippy | `cargo clippy --workspace --all-targets --all-features -- -D warnings` | 通过 |
| Workspace 与属性测试 | `cargo test --workspace` | 修复后全部通过，含持久化失败 seed |
| Python oracle | `python tests/oracle/runner.py` | 122 个 fixture：118 成功、4 个预期错误（含 20 个往返 fixture） |
| Rust 差分 | `cargo test -p docxtpl-rs --features oracle` | 102 个渲染用例：98 字节匹配、4 错误类别匹配 |
| MSRV | `cargo +1.85.0 check --workspace --locked` | 通过 |
| 性能与资源 | release 构建后 `python tests/p7c_audit.py --iterations 15` | 127 模板资源指标在限额内；3 个样例完成测量 |
| Office | 构建 CLI 后 `python tests/office_check.py` | LibreOfficeDev 26.8.0.0.alpha0 打开、另存、ZIP/XML 表格结构断言 8/8 |

## 实测发现与修复

初次 workspace 属性测试发现最小输入 `<!aAῖપ` 导致严格解析器 panic：
DOCTYPE 判定直接对字符串取前 7 字节，切到 Unicode 字符中间。
现改为字节前缀比较，仅在匹配 ASCII `DOCTYPE` 后读取后续字符。
加入顶层与元素内畸形声明的定向回归测试，并保存 proptest 失败 seed。
修复后完整 workspace、oracle、Clippy、MSRV 与 Office 检查通过。

首次 oracle 环境缺少 P6 所需 `docxcompose`，补齐固定依赖后重跑通过；
该环境失败不计为产品通过记录。

## 性能基线

三个样例各 15 次，render 中位数依次为 7.416 / 6.806 / 15.102 ms；
峰值 RSS 依次为 22,659,072 / 18,956,288 / 9,158,656 B。
详见 `p7c-performance-macos.json`。这是本机首份基线，未与 Windows/Linux
数据比较，不能据此宣称性能回退检查通过。

机器可读汇总和日志 SHA-256 见 `p7d-macos-verification.json`；完整日志保留在
本机 `target/macos-verification/`。本轮 Office 验证为 headless 打开与另存，
未新增 WPS 或 Word 人工外观检查。长期 fuzz corpus 与 sanitizer 趋势仍待积累。

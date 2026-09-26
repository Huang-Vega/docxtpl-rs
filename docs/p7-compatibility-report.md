# P7 兼容性验收报告（0.8.0）

日期：2026-09-26
参考实现：Python docxtpl 0.20.2

## 结论

P7 功能冻结范围通过：102 个 render fixture 中 98 个输出逐 part 原始字节一致，
4 个预期错误的类别一致；声明功能通过率 100%，阻断用例通过率 100%。
P7c 在 Windows 与 Ubuntu 24.04 x86_64 VM 均未发现 panic、资源限额缺口或
已知高危包校验缺陷；Linux 上 LibreOffice 24.2.7.2 的 8 个代表性打开/另存
检查通过，Windows 上 Microsoft Word 16.0.17932.20700 x64 对 10 个 Rust
实际输出的打开、另存、重开与 11 页人工外观检查全部通过。版本候选为 0.8.0；
macOS 实跑与长期 fuzz corpus 仍待补齐，因此不发布 registry 包、不创建 Git 标签。

## 验收证据

| 门禁 | 结果 |
|---|---|
| Python oracle 差分 | 102/102；98 MATCH + 4 错误类别一致 |
| patch/recover golden | 102/100 全部一致 |
| 属性测试 | 6/6 性质通过（每项 256/512 cases） |
| 资源审计 | 127 模板全部在默认限额内 |
| 性能基线 | 3 个 release 内部基准已拆分 open/render/write，并记录峰值 RSS；20% 阈值仅适合同机复测 |
| 跨平台 | Windows 与 Ubuntu 24.04 x86_64 实测通过；macOS 结果待 CI |
| LibreOffice | 24.2.7.2 headless 打开并另存 8/8，通过表/行/单元格结构断言 |
| Microsoft Word | ProPlus2024Volume 16.0.17932.20700 x64：代表性 Rust 输出打开/另存/重开 10/10，Word 导出页面人工检查 11/11，重存 DOCX ZIP/XML 结构检查 10/10 |
| Rust 质量门禁 | workspace test、clippy `-D warnings`、fmt |

性能和资源明细见 Windows `p7c-performance-baseline.json`、Linux
`p7c-performance-linux.json` 与 `security-limits.md`。
Microsoft Word 的 fixture、哈希、文档计数与逐项观察见 `p7d-word-smoke.json`。
完整功能分层与 DEV-0001～DEV-0014 公开边界见 `compatibility.md`。

## 遗留边界

不承诺任意 Python 对象、自定义 Jinja2 环境/扩展、corpus 外的 libxml2 recover
长尾，以及 compatibility.md 已列出的脚注图片、endnotes 和若干 Subdoc 长尾。
这些边界均为公开的 documented-deviation/unsupported，不计入冻结分母。

## 尚未闭环的加固证据

- 已增加 OPC、strict/recover XML、`patch_xml` 与完整 from_bytes/render 四个
  `cargo-fuzz` target，并配置每周有界运行；长期 corpus 与 sanitizer 趋势仍需 CI 积累。
- 性能基线已排除 CLI 启动并拆分打开/渲染/写出；Windows、Linux、macOS
  均可读取进程峰值 RSS，基线比较会拒绝跨平台数据。
- DEV-0007 的重复/外部/孤立 story 与 endnotes、DEV-0008 借用模式 API 边界、
  DEV-0009、DEV-0010 的 `w:nsid`/主包缺 numbering/编号重启分支，以及
  DEV-0011、DEV-0012 均已有动态 DOCX 或 compile-fail 回归。
- macOS 实跑结果仍待 CI。

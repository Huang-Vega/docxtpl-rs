# ADR-010：P7c 兼容加固与 P7d 0.8.x 冻结

日期：2026-09-26
状态：已接受
阶段：P7c（fuzz/property、限额、性能、跨平台）与 P7d（兼容报告/0.8.x）

## 背景

P7a/P7b 及后续补漏已把公开功能和真实 Word corpus 收敛到 102 个 render fixture
逐字节全绿，但 P7 验收还要求畸形输入无 panic、资源限额复核、性能回归和
跨平台门禁。上述项目不改变 docxtpl 兼容语义，因此不新增 oracle fixture。

## 决策

1. 对三个高风险纯入口增加 proptest：OPC 随机/截断字节、XML strict/recover
   任意 UTF-8 与 XML-like 输入、`patch_xml` 任意与 marker-heavy 输入。
   case 数固定为 256/512，使每次 workspace test 都可复现且耗时有界。
2. `tests/p7c_audit.py` 扫描冻结模板的 ZIP 中央目录并记录峰值；release 内部
   基准对基础变量、复合表格、真实 Word 动态表格各测 15 次，拆分记录
   open/render/write、输出大小与峰值 RSS。同平台 `--compare` 的 render
   回退超过 20% 返回失败。
3. 性能基线是机器相关证据，不跨平台硬比较；本地 `--compare` 只适合同机
   复测。跨平台正确性由 Windows、Linux、macOS CI 承担，Python oracle 与
   LibreOffice 固定在 Linux job；CI 未运行前不得把配置写成实测通过。
4. P7 公开范围以 `docs/compatibility.md` 的 compatible 与 DEV-0001～0014
   为准。102 个 render fixture 中 98 个逐字节 MATCH、4 个错误类别一致，
   即声明范围 100%，阻断用例 100%，高于 95% 门槛。
5. workspace 版本候选为 0.8.0；Office 代表性实机抽查已由 LibreOffice 与
   Microsoft Word 覆盖，macOS 实跑已补齐，发布前仍需长期 fuzz 证据。峰值 RSS
   采集支持 Windows/Linux/macOS。发布到 registry、创建 Git tag 属外部发布
   动作，不在本 ADR 内。

## 实测结果

- corpus：127 个模板；最大压缩包 38 750 B、23 条目、单条目 438 131 B、
  总解压 833 014 B、压缩比 32.156，均显著低于默认限额。
- Windows AMD64 / Python 3.13.13 release 分阶段基线（各 15 次）：
  `r2_var_basic` render 15.699 ms，`r3_combo_invoice` 17.799 ms，
  `p7b_dynamic_table` 27.610 ms；详见 `docs/p7c-performance-baseline.json`。
- Ubuntu 24.04 x86_64 / rustc 1.98.1 / Python 3.12.3 实测 workspace、clippy、
  fmt 与 oracle 全绿；LibreOffice 24.2.7.2 打开并另存 8/8。Linux release
  分阶段 render 基线依次为 8.409 / 9.869 / 20.747 ms，详见
  `docs/p7c-performance-linux.json`。
- Windows 11 Pro x64 / Microsoft Word 16.0.17932.20700 x64 对 10 个代表性
  Rust 输出完成打开、另存与重开，10/10 通过；Word 导出的 11 页经人工外观
  检查 11/11 通过，重存 DOCX ZIP/XML 结构检查 10/10。明细见
  `docs/p7d-word-smoke.json`。
- 新增 6 个 property tests 全部通过；完整门禁与 oracle 结果记录在
  `docs/p7-compatibility-report.md`。

- macOS 26.6.2 arm64 / rustc 1.98.1 / Python 3.14.4 全部门禁通过，
  LibreOfficeDev 26.8.0.0.alpha0 打开并另存 8/8；修复属性测试发现的
  strict `<!` 分支 Unicode 切片 panic。详见 `docs/p7d-macos-verification.md`
  与 `docs/p7c-performance-macos.json`。

## 影响

不改变库运行时行为。测试新增 `proptest` 仅为 dev-dependency；性能脚本只在
项目 `target/` 下构建和创建临时输出。后续 P8 可复用 `--compare` 检查发布候选。

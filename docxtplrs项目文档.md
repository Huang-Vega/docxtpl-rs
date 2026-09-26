# docxtpl-rs 项目规划文档

> 启动版 · 2026-09-24。工期按一名熟悉 Rust 的全职开发者估算，P0 后依据审计结果重估。

## 1. 目标与原则

目标是将 Python docxtpl 以**保留语义的 Rust 移植（semantic port）**方式实现为 docxtpl-rs，使现有模板、上下文和渲染选项尽可能产生等价的 Word 文档。移植对象是模板语义与处理流水线，而非逐行翻译 Python 或复制其对象模型。

- [ ] 固定一个 Python docxtpl 版本、提交 SHA 与依赖锁文件作为参考实现；候选基线为 0.20.2，P0 最终确定。
- [ ] 保留“OOXML 预处理 → 模板渲染 → OOXML 后处理 → OPC 包写出”的语义流程。
- [ ] 每项功能登记为 compatible、documented-deviation 或 unsupported；任何偏差都有最小复现模板。
- [ ] 默认兼容上游的转义、标签删除及错误语义；更严格的安全选项单独配置。
- [ ] 只实现 docxtpl 需要的 OOXML/OPC 子集，不先重造完整 python-docx。
- [ ] 对不可信 DOCX 设置 ZIP 项数、压缩比、解压总量、XML 深度、输出大小和渲染时间等限额。

判定等价以**可见内容、段落/表格结构、格式引用、媒体和 relationship 图**为主。ZIP 字节、时间戳、属性顺序及可重映射的关系 ID 无须相同。不能为追求高通过率而忽略悬空关系或有语义的空白。

## 2. 兼容范围、MVP 和非目标

| 边界 | 必须完成 | 暂不承诺 |
|---|---|---|
| MVP：P0–P3，0.1.0-alpha | 固定基线；读写 DOCX；正文普通变量、if/for、已选定的过滤器、转义；结构化 p/tr/tc/r 标签；表格行循环 | RichText、图片、多 part、subdoc |
| 常用功能版：P4–P5，0.2.x | RichText/Listing、InlineImage、超链接、页眉页脚、脚注及核心属性中经审计的常用行为 | 子文档合并 |
| 高度兼容版：P6–P7，0.8.x | subdoc、图片替换、高级表格及审计清单内的边界行为；大规模差分回归 | 任意 Python 对象、自定义 Jinja2 扩展自动兼容 |
| 稳定版：P8，1.0.0 | 公布范围通过质量门禁，剩余差异逐项公开 | 未来所有上游版本 100% 兼容 |

非目标：完整 Word 排版引擎；完整 python-docx；逐字节相同的 ZIP；执行 Python 代码；首版宏文档、加密文档和所有第三方 OOXML 扩展。

**跨 run 边界**：上游文档要求普通 Jinja 标签位于同一 run；Word 又可能拆分用户可见文本。P0 要用参考实现实测跨 w:t、跨 w:r、夹杂格式/书签时的结果。兼容模式只承诺经 oracle 证实的行为；超出上游的自动修复作为显式可选能力，不能计入上游兼容率。

## 3. 总体架构

    DOCX + Context
       │
       ▼
    ZIP/OPC 读取与限额校验
       │
       ▼
    Part / ContentType / Relationship 索引
       │
       ▼
    WordprocessingML token 定位与结构化标签归一化
       │
       ▼
    MiniJinja 渲染 + RichText/图片等类型化值适配
       │
       ▼
    XML 后处理：表格、空白、ID、引用
       │
       ▼
    Part 与关系更新 → ZIP 写出 → 包完整性检查

分层规则：OPC 层不理解 Jinja；模板层不直接操作 ZIP；兼容层收纳上游的特殊修补规则。正文、页眉、页脚与脚注复用 render_part，但各自使用独立关系作用域。未修改的 part 尽量原样保留。错误信息带 part、结构路径和源 token 位置。

## 4. crate / 模块拆分

初期可先以 workspace 内模块实现，P2 边界稳定后拆成独立 crate；对外只暴露一个主库 API。

| crate / 模块 | 责任 | 首次落地 |
|---|---|---|
| docxtpl-rs | 公开 DocxTemplate、RenderOptions、错误与返回文档 | P2 |
| docxtpl-opc | ZIP、part URI、Content Types、rels、media 分配与包验证 | P1 |
| docxtpl-xml | XML 事件/可编辑片段、命名空间、文本视图、源位置映射 | P1–P2 |
| docxtpl-template | 标签预处理、MiniJinja 适配、part 渲染、后处理 | P2–P3 |
| docxtpl-rich | RichText、Listing、InlineImage、Subdoc 等类型化值 | P4–P6 |
| docxtpl-compat | 上游版本行为表、偏差和诊断码 | P0 起 |
| docxtpl-cli | render、inspect、compat-check 命令 | P2/P8 |
| tests/oracle | Python runner、fixture、XML 规范化与语义差分 | P0 |

建议目录：

    docxtpl-rs/
    ├── Cargo.toml
    ├── Cargo.lock
    ├── crates/
    │   ├── docxtpl-rs/src/
    │   ├── docxtpl-opc/src/
    │   ├── docxtpl-xml/src/
    │   ├── docxtpl-template/src/
    │   ├── docxtpl-rich/src/
    │   ├── docxtpl-compat/src/
    │   └── docxtpl-cli/src/
    ├── tests/
    │   ├── fixtures/{templates,contexts,expected}/
    │   ├── oracle/{runner.py,canonicalize.py,compare.py}/
    │   ├── integration/
    │   └── fuzz/
    ├── benches/
    ├── docs/{compatibility.md,architecture.md,decisions/}
    └── .github/workflows/

## 5. Python docxtpl → Rust 对应关系

| Python 侧 | Rust 侧 | 验证对象 |
|---|---|---|
| DocxTemplate.render | DocxTemplate::render + RenderOptions | 上下文、autoescape、错误语义 |
| patch_xml | template::normalize_tags + compat::rules | 结构标签、实体、表格修补 |
| render_xml_part | template::render_part + MiniJinja | 各 XML part 的输出 |
| resolve_listing、fix_tables、fix_docpr_ids | xml::postprocess | 换行/制表、网格、ID 唯一性 |
| python-docx 的 package/part/rels | opc::{Package,Part,Relationship} | 引用图与未知 part 保留 |
| lxml | quick-xml + 专用可编辑片段层 | 节点顺序、命名空间、xml:space |
| Jinja2 | MiniJinja + 兼容过滤器/测试 | 已声明语法子集与偏差 |
| RichText、InlineImage、Subdoc | rich 中对应 Rust 类型 | 样式、媒体和 rels |
| zipfile | zip + 自建 OPC 索引 | DOCX 包完整性 |

P0 需审计上游源码中未在此表列出的入口。上游还处理核心属性、脚注、图片替换等，不能只按公开 API 名称估算工作量。

## 6. 技术选型与待定决策

| 层 | 候选 | 决策要求 |
|---|---|---|
| ZIP/OPC | [zip](https://docs.rs/zip/latest/zip/) + 自建 OPC 模型 | 验证未修改项保留、重复项、ZIP64 和限额；锁定具体版本 |
| XML | [quick-xml](https://docs.rs/quick-xml/latest/quick_xml/) 读写事件；自建可编辑片段 | 不依赖全局字符串替换；验证命名空间、实体及空白往返 |
| 只读校验 | [roxmltree](https://docs.rs/roxmltree/latest/roxmltree/) | 适合测试和结构检查，不作为主编辑器 |
| 模板 | [MiniJinja](https://docs.rs/minijinja/latest/minijinja/) | 与 Jinja2 对照过滤器、测试、undefined、autoescape、空白控制 |
| 上下文 | serde / serde_json + RenderValue | MVP 接受 JSON；富内容使用显式类型 |
| 质量工具 | thiserror、proptest/fuzz、语义 diff | 错误能定位到 part/段落/标签 |

- [ ] ADR-001：固定上游版本及许可证；上游元数据声明 LGPL-2.1-only，移植代码前核实归属与许可义务。
- [ ] ADR-002：XML 事件流、轻量树或混合编辑模型及未知节点保留策略。
- [ ] ADR-003：MiniJinja/Jinja2 支持矩阵；不笼统宣称完全兼容。
- [ ] ADR-004：compat/strict 模式、安全限额和错误政策。

## 7. 关键难点与首个可执行任务

| 难点 | 任务节点 | 必验边界 |
|---|---|---|
| 跨 w:r / w:t 模板 token | 建带节点位置的逻辑文本视图；用 oracle 决定是否及如何重建 | 不跨段落误拼；不吞格式、书签、批注 |
| p/tr/tc/r 结构化标签 | 标签提升至相应结构边界并删除承载元素 | 嵌套控制、空集合、重复标签、表格宽度 |
| RichText / RichTextParagraph | 类型化片段生成 w:r/w:p | 样式继承、字体区域、超链接、上游过滤器限制 |
| Listing | 复现换行、制表、换段、换页映射 | xml:space 和前后空白 |
| 图片与 media | 格式/尺寸识别、唯一文件名、绘图 ID 分配 | 同图复用、缺图、图片替换、Content Types |
| relationship | 每个 part 内独立生成 rId，绑定目标路径 | 悬空引用、重复 ID、外部 URL |
| header/footer/footnotes | 复用渲染器并正确写回 part/rels | 多 section、共享 part、页眉图片 |
| subdoc | 合并 body、样式、编号、media、rels 并重映射 ID | 样式/编号冲突、嵌套表格、重复插入 |
| 安全与错误位置 | 限额、实体策略、源位置映射 | ZIP bomb、畸形 XML、深层嵌套 |

## 8. 分阶段里程碑

下表的工期为单人串行粗估。每阶段以验收标准达成作为合并门槛；未通过项必须转为有最小模板的 issue。

| 阶段 | 任务 checklist | 产出 | 验收标准 | 依赖 | 风险 | 工期 |
|---|---|---|---|---|---|---|
| **P0 源码审计与测试基线** | ☐ 固定 tag/SHA/依赖；☐ 盘点 template、richtext、inline_image、subdoc、测试；☐ 建功能清单；☐ 建 oracle | compatibility.md、基线环境、fixture 清单 | ≥30 个代表性用例可复现，均有阶段/语义标记 | 无 | 系统字体、fixture 许可、上游隐藏行为 | 1–2 周 |
| **P1 OOXML package** | ☐ ZIP 读写；☐ part/type/rels 索引；☐ URI 校验；☐ 保留未知 part；☐ 限额 | docxtpl-opc、往返测试 | ≥20 个模板无渲染往返通过包校验，未知 part 不丢、关系无悬空 | P0 | ZIP 变体、相对路径、编码 | 2–3 周 |
| **P2 基础模板渲染** | ☐ 正文变量/if/for；☐ token 映射；☐ 转义/空白；☐ 错误定位 | 最小 API、CLI 原型 | P2 fixture 与 oracle 语义一致；输出 XML 有效且 Word/LibreOffice 可打开 | P1 | Jinja 差异、token 被 Word 拆分 | 2–4 周 |
| **P3 结构化标签与表格循环** | ☐ p/tr/tc/r；☐ 行/单元格复制；☐ 网格/合并修复；☐ 嵌套 | **MVP 0.1.0-alpha**、指南 | 空/单/多元素循环正确；无标签承载残留；表格可编辑 | P2 | 上游正则修补的边界 | 3–5 周 |
| **P4 RichText 与图片** | ☐ RichText/RichTextParagraph；☐ Listing；☐ InlineImage；☐ media/ID | 富内容 API、回归集 | 格式、链接、图片在两种办公软件可见；尺寸、媒体和 rels 正确 | P3，P1 的关系基础 | 字体、图片尺寸、重复 ID | 3–5 周 |
| **P5 header/footer/relationships** | ☐ 页眉页脚；☐ 脚注/核心属性；☐ 外链/内部关系；☐ 多 section | 多 part 渲染、0.2.x | 各 part 与 oracle 语义一致；全部 r:id 有有效目标 | P4 | 共享 part 重复写入、关系作用域 | 2–4 周 |
| **P6 subdoc 及高级能力** | ☐ subdoc 合并；☐ 样式/编号/media/rels 重映射；☐ 图片/嵌入替换 | subdoc API、高级兼容清单 | 复合样例无 ID/样式冲突；Word/LibreOffice 可打开编辑 | P5 | docxcompose 隐含语义 | 4–7 周 |
| **P7 compatibility hardening** | ☐ 扩大 corpus；☐ fuzz/property；☐ 诊断和性能；☐ 关闭阻断差异 | 0.8.x、兼容报告 | 已声明功能分层差分通过率 ≥95%；阻断用例 100%；无已知高危包校验缺陷 | P6 | 长尾模板、性能退化 | 3–6 周 |
| **P8 发布** | ☐ API/许可证/MSRV 审核；☐ README/迁移指南；☐ release CI；☐ 发布说明 | 1.0.0 候选与 crate/CLI | 全部门禁通过；干净环境安装运行；兼容范围与偏差公开 | P7 | API 过早稳定、依赖升级 | 1–2 周 |

串行合计约 **21–38 周**。P0 若发现范围更大，应修订工期和 1.0.0 兼容声明，不能靠删除难例达到验收率。

### 阶段执行 checklist

以下是里程碑表中任务的 GitHub issue 粒度拆分；每项应有负责人、关联 fixture 和验收记录。

**P0**

- [ ] 固定上游 tag/SHA、Python 依赖与可复现运行环境。
- [ ] 审计核心源码、公开 API、文档与现有测试，形成兼容清单。
- [ ] 建立至少 30 个 fixture、Python runner 和首版 semantic diff。

**P1**

- [ ] 实现 ZIP 读写、part URI、Content Types 和 relationship 索引。
- [ ] 实现未知 part 保留、包校验与输入限额。
- [ ] 完成至少 20 个模板的无渲染往返测试。

**P2**

- [ ] 实现正文 token 定位、变量、if/for 和 MiniJinja 适配。
- [ ] 实现转义、空白处理与 part/标签位置诊断。
- [ ] 打通最小公开 API、CLI 与端到端差分。

**P3**

- [ ] 实现 p/tr/tc/r 标签的结构边界提升与承载元素删除。
- [ ] 实现表格行/单元格循环、嵌套与网格修复。
- [ ] 冻结 MVP 兼容清单，发布 0.1.0-alpha。

**P4**

- [ ] 实现 RichText、RichTextParagraph 和 Listing。
- [ ] 实现 InlineImage、media 分配和图形 ID 去重。
- [ ] 完成字体、样式、超链接与图片的差分及打开检查。

**P5**

- [ ] 复用 render_part 处理页眉、页脚、脚注和核心属性。
- [ ] 完成 part 内关系作用域、外部链接和多 section 用例。
- [ ] 发布 0.2.x 的功能矩阵与已知偏差。

**P6**

- [ ] 合并 subdoc 的正文、样式、编号、media 和 rels。
- [ ] 实现图片/嵌入替换及经审计的高级能力。
- [ ] 用复合文档验证 ID 重映射与两种办公软件可编辑性。

**P7**

- [x] 扩充冻结的差分 corpus，按功能统计通过率。
- [ ] 跑长期 fuzz、峰值内存、三平台和性能回归（Windows 有界 proptest/资源/CLI 基线已完成）。
- [ ] 关闭全部阻断差异，发布兼容报告及 0.8.x（功能报告已冻结，待 CI/Office 证据后发布）。

**P8**

- [ ] 审核 API、MSRV、许可证、依赖和发布产物。
- [ ] 完成 README、示例、迁移指南、变更日志与发布 CI。
- [ ] 在干净环境验证安装和示例，发布 1.0.0 候选。

## 9. Python reference implementation 差分测试

**输入**：同一 template.docx、context.json 和渲染选项分别交给固定版本的 Python oracle 与 Rust 实现。RichText、图片和 subdoc 用 fixture manifest 指定构造方法，因为普通 JSON 不能表达 Python 对象。

1. 固定 docxtpl、Jinja2、python-docx、lxml、docxcompose 版本及运行镜像，记录 SHA 和环境摘要。
2. 两端生成 DOCX；失败时比较错误类别、part/位置及已登记偏差。
3. 解压 DOCX，先核对 part 列表、Content Types 和 relationship 图。
4. 对 XML 做 namespace-aware canonicalization：规范命名空间表达与属性顺序，保留文本顺序、有语义的空白、xml:space、表格和样式引用。
5. 只对有证据的非语义字段做限定归一化：ZIP 时间戳、按类型/目标重映射的 rId、随机绘图 ID；不得全局删除所有 ID。
6. semantic diff 按 part → 段落/表格路径 → run/文本/样式/引用/媒体报告差异；媒体比较哈希和可解码元数据。
7. 关键样例增加 Word/LibreOffice 打开检查、文本抽取和人工外观抽检。外观检查补充 XML 校验。

fixture manifest 最少记录 id、feature、上游版本、输入、预期状态、允许的归一化、已知差异、负责人和 issue。回归失败不得直接加入全局忽略规则。

## 10. 测试矩阵、CI/CD 与 benchmark

| 维度 | 最小覆盖 | 方法 |
|---|---|---|
| 模板语法 | 变量、表达式、filter、if/for、空集合、嵌套、转义、undefined | oracle 差分 |
| Word XML 形态 | 单 w:t、跨 w:t、跨 w:r、格式切换、书签/批注、xml:space | 生成型 fixture |
| 表格 | 行/单元格循环、嵌套表、横纵合并、gridSpan | 差分与结构断言 |
| 富内容 | RichText、字体/样式、Listing、超链接、JPEG/PNG、同图复用 | 媒体与关系检查 |
| 多 part | body、header、footer、footnote、core properties、subdoc | part 级差分 |
| 负面输入 | 破损 ZIP、重复项、非法关系、无效 XML、压缩炸弹、深层嵌套 | 单元测试与 fuzz |
| 平台 | Linux/macOS/Windows，稳定 Rust 与 MSRV | CI；发布期 Office 抽检 |

CI/CD 门禁：每个 PR 跑格式化、Clippy、单元/集成测试、固定小型 oracle corpus、依赖/许可证扫描；夜间跑完整差分、fuzz 种子与 benchmark；候选发布跑三平台、MSRV、全部 fixture 和 DOCX 打开检查。报告按功能显示通过、偏差和未支持数量。

Benchmark：

- [ ] 建立 1 页纯文本、20 页表格循环、含 100 张图片或多 part 报告三组模板。
- [ ] 固定机器/输入/预热/样本数，分开测打开包、预处理、渲染、后处理、压缩写出、峰值 RSS 与输出体积。
- [ ] 与 Python oracle 比较；P2 建基线，P4/P6/P7 复测。性能回退超过 20% 且无解释时阻断合并；发布性能目标以 P0/P2 实测确定。
- [ ] 检查大输入与深嵌套循环下的资源上限和意外平方增长。

## 11. API 草案

    use docxtpl_rs::{DocxTemplate, RenderOptions};
    use serde_json::json;

    fn main() -> Result<(), Box<dyn std::error::Error>> {
        let tpl = DocxTemplate::open("template.docx")?;
        let doc = tpl.render(
            &json!({"name": "Vega", "items": [{"name": "Apple"}]}),
            RenderOptions::compat(),
        )?;
        doc.save("output.docx")?;
        Ok(())
    }

DocxTemplate 建议只读可复用，每次 render 返回独立文档。RenderOptions 显式包含 autoescape、未定义值策略、资源限额和可选的跨 run 宽松修复。P4 后增加 RenderContext/RenderValue，承载 RichText、InlineImage 等非 JSON 值。P2 再通过 ADR 冻结 alpha API；上述签名只是启动草案。

## 12. 风险清单

| 风险 | 早期信号 | 处置 |
|---|---|---|
| MiniJinja/Jinja2 差异 | 同输入文本或错误不同 | P0 语法矩阵；兼容适配；公开不支持项 |
| XML 编辑损伤格式 | Word 可打开但样式变化 | 源节点映射、结构/外观双检查 |
| subdoc 合并失控 | 编号、样式、关系冲突 | P6 独立试验；先覆盖常用合并路径 |
| ZIP/XML 资源耗尽 | 内存或时间暴涨 | 默认限额、压缩比限制、fuzz、禁外部实体 |
| 许可或 fixture 不能再分发 | 发布门禁受阻 | P0 核实 LGPL-2.1-only、代码来源和素材授权 |
| 上游版本漂移 | 新版本改变行为 | 固定基线；升级独立里程碑和差分报告 |
| 字体与 Office 差异 | 截图不同 | 以 OOXML 语义为主，固定环境人工抽检 |

## 13. Definition of Done

一个功能完成需同时满足：

- [ ] 兼容清单中有支持级别、上游版本、语义描述和最小模板。
- [ ] 有正常、边界和失败样例；差分通过或偏差经过审阅。
- [ ] 输出 ZIP、XML、relationship、Content Types 和引用 ID 校验通过。
- [ ] API/CLI 文档、错误信息、示例和限制已更新。
- [ ] CI、资源限额检查与性能门禁通过。
- [ ] 代码审查能从测试回溯至上游行为或设计决议。

MVP 完成还要求 P0–P3 验收全过，并明确标注富文本、图片、多 part、subdoc 暂不支持。高度兼容完成要求 P0–P7 声明范围达成差分门禁，剩余偏差逐项公开；95% 仅针对预先冻结且按功能分层的 corpus。

## 14. 推荐实施顺序与首批 GitHub issue

1. [ ] 建仓库、许可证评估、贡献指南、issue/PR 模板、workspace 与 CI 骨架。
2. [ ] 固定 Python oracle 和依赖，整理至少 30 个代表性 fixture，并按功能分类。
3. [ ] 完成 P1 ZIP/OPC 往返，先确保未修改模板不会被破坏。
4. [ ] 用 5 个最小模板做纵向切片：读包 → 渲染变量 → 写包 → semantic diff。
5. [ ] 加入 if/for、转义、错误位置，再实现结构化标签与表格循环，形成 MVP。
6. [ ] 按 P4–P6 扩展富内容、多 part 和 subdoc；每新增功能先加入 oracle fixture。
7. [ ] P7 收敛差异并冻结公开兼容声明，P8 发布稳定版。

## 参考资料

- [Python docxtpl 源码仓库](https://github.com/elapouya/python-docx-template)与[核心 template.py](https://github.com/elapouya/python-docx-template/blob/master/docxtpl/template.py)：P0 审计时固定 tag/SHA。
- [Python docxtpl 官方文档](https://docxtpl.readthedocs.io/en/latest/)：结构化标签、RichText、图片、subdoc 和转义。
- [上游项目元数据与许可证声明](https://github.com/elapouya/python-docx-template/blob/master/pyproject.toml)。
- Rust 候选库官方文档：[MiniJinja](https://docs.rs/minijinja/latest/minijinja/)、[quick-xml](https://docs.rs/quick-xml/latest/quick_xml/)、[zip](https://docs.rs/zip/latest/zip/)、[roxmltree](https://docs.rs/roxmltree/latest/roxmltree/)。

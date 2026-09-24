# ADR-002：XML 编辑模型——上游同构的有界字符串管线 + 宽松愈合解析

- 状态：已接受（P0）
- 决策日期：2026-09-24

## 背景

审计上游 0.20.2 后确认（`third_party/docxtpl-0.20.2/docxtpl/template.py`）：

- `patch_xml` 全部为**有界**字符串正则变换（限定在 jinja 标签内或单个承载元素内），且带嵌套守卫（如 `<w:p[ >](?:(?!<w:p[ >]).)*`）。
- 拆分标签的合并（步骤 A/B）会刻意删除标签两侧的 XML 标记，产出**局部非法**的字符串。
- 渲染默认 `autoescape=False`：上下文值**原样**插入 XML；非法字符依赖 `fix_tables` 中的
  `etree.fromstring(xml, parser=XMLParser(recover=True))` 宽松解析“愈合”，再由 python-docx 重新序列化。
- 因此上游的可观察输出 = 字符串手术结果经 libxml2 recover 解析 + lxml 重序列化后的产物。

若采用纯 XML 树实现，会产生“更正确但不同”的输出（例如值中裸 `&` 被提前转义、合并标签不破坏结构），
导致 documented-deviation 蔓延，违背规划文档的兼容优先级。

## 决策

采用与上游**同构**的管线（`docxtpl-template`）：

1. 提取 `w:body`（含根上可见的 xmlns 声明）为字符串，复刻 `lxml.tostring(body)` 的行为。
2. `docxtpl-compat` 用 fancy-regex 逐条移植 patch_xml 的 12 个正则（前瞻/后顾/守卫保持原样）。
3. MiniJinja 渲染（默认 autoescape=false，对齐上游）。
4. `resolve_listing` 同样以有界正则移植。
5. **宽松解析**（`docxtpl-xml`，自研）模拟 libxml2 recover 在本 corpus 上的关键行为：
   - 文本中裸 `&`（非合法实体）保留为字符数据，序列化时再转义；
   - `<name` 形式且后随合法名称字符时按元素处理（值注入 `<b>` 产生真实元素）；
   - 失配的闭合标签、悬挂的结构由解析器丢弃/修复，并记录诊断（不静默）；
   - 具体边界以 oracle fixture 钉死（r2_value_* 系列）。
6. 树级修复 `fix_tables` / `fix_docpr_ids` 按命名空间 URI 匹配（等价上游 `nsmap` 用法）。
7. 错误带 part 名、行号与 `docx_context` 式的文本片段（对齐上游 `exc.docx_context`）。

## 与质量规范 §4.2 的关系

规范禁止的是“无边界的字符串替换”。本管线每一步都有明确的作用域（标签内 / 单元素内 / 单 w:t 内），
并以严格/宽松双模解析器做结构校验与诊断输出，不依赖“似乎能打开”的静默恢复；愈合动作全部记录。

## 后果

- 未被 fixture 钉死的 recover 长尾行为按保守策略处理并登记偏差（DEV-0002）。
- 未来“strict 模式”（提前转义、拒绝非法输入）作为显式选项另行设计，不计入兼容率。

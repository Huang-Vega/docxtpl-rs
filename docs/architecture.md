# docxtpl-rs 架构

> 与规划文档 §3 一致；本文件记录 P0 决策后的实际形态（ADR-002/-003/-004）。

## 数据流

    DOCX + Context
      │
      ▼
    docxtpl-opc：ZIP/OPC 读取（限额、URI 校验、part/rels/content-types 索引）
      │
      ▼
    docxtpl-rs 门面：定位主文档 part，提取 w:body（含继承 xmlns）
      │
      ▼
    docxtpl-template：
      patch_xml（docxtpl-compat 正则，12 步有界变换）
      → MiniJinja 渲染（默认 autoescape=false）
      → resolve_listing
      → docxtpl-xml 宽松(recover)解析愈合（诊断记录）
      → fix_tables / fix_docpr_ids（命名空间 URI 匹配）
      → 序列化植回 document.xml
      │
      ▼
    docxtpl-opc：part 更新 → ZIP 写出（未修改 part 原样保留）→ 包完整性检查

## 模块边界与依赖方向

    docxtpl-rs（门面） → docxtpl-template → docxtpl-xml / docxtpl-compat
                        docxtpl-rs       → docxtpl-opc → quick-xml（仅解析 rels/content-types）
    docxtpl-cli → docxtpl-rs

- opc 不理解 Jinja；xml 不触碰 ZIP；compat 只收纳有上游证据的规则（上游正则逐条移植）。
- tests/oracle（Python）仅为测试基础设施，运行时 crate 不依赖。
- 每次渲染独立状态：DocxTemplate 可复用，render 返回新的 RenderedDocument。

## 关键不变量

1. 未修改 part 原样保留（DEV-0004）。
2. 渲染产物必须通过宽松解析 + 包校验；愈合动作全部产生诊断。
3. 错误信息携带 part 名、行号与文本上下文（对齐上游 docx_context）。
4. wp:docPr id、rId 等标识符作用域：body 全局重编号仅 docPr（对齐上游）；
   rels 修改仅发生在显式功能（P4+）。

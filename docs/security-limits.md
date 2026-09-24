# 安全限额（security-limits）

> 状态：P1 corpus 校准于 2026-09-25 完成；数值适用于默认配置。
> 修改本文件必须随 PR 更新测试（代码规范 §1.3 / §6）。

## 输入限额（docxtpl-opc::PackageLimits，默认值）

| 项 | 默认值 | 说明 |
|---|---|---|
| `max_entries` | 1 000 | ZIP 条目数 |
| `max_entry_uncompressed` | 32 MiB | 单条目解压后大小 |
| `max_total_uncompressed` | 128 MiB | 全部条目解压总量 |
| `max_compression_ratio` | 200 | 解压后/压缩后（压缩炸弹拦截；stored 条目不受限） |
| `max_output_size` | 128 MiB | 写出包大小（计数写包装器强制） |

门面另限制压缩输入 DOCX 为 128 MiB，单个渲染后的 XML 为 64 MiB，
MiniJinja 每次模板求值为 10 000 000 fuel。超限返回错误。

## XML 限额（docxtpl-xml）

| 项 | 默认值 |
|---|---|
| 最大嵌套深度 | 512 |
| 外部实体/DTD | 禁用（XXE） |

## URI 规则

- 拒绝：绝对路径（盘符/前导 `/`）、`..` 段、反斜杠、空条目名。
- 重复检测三口径：精确、大小写折叠、百分号解码折叠。
- rels 内部目标解析后必须落在包内（`validate()` 校验，悬空即错）。

## 已知安全测试（P1 起）

- 压缩炸弹（高压缩比条目）、条目数超限、总解压超限、单条目超限。
- 非法路径（`../evil.txt`、绝对路径）、重复条目名、截断 ZIP。
- 深层嵌套 XML、非法实体、超大文本节点。

## P1 实测依据与复核

对 `tests/fixtures/templates` 的 69 个 DOCX 逐个读取 ZIP 中央目录和 XML：

| 指标 | corpus 最大值 | 对应样本 |
|---|---:|---|
| 压缩文件 | 37 650 B | rt_header_footer |
| 条目数 | 19 | rt_header_footer |
| 单条目解压量 | 438 131 B | rt_rsid 等 |
| 总解压量 | 830 652 B | rt_long |
| 单条目压缩比 | 32.16 | rt_rsid 等 |
| XML 深度 | 12 | r3_docpr |

新默认值为 corpus 留出较大余量，同时比 ADR-004 初值收紧了条目数和
内存相关上限。此 corpus 均为小型生成模板，无法代表真实大型文档；接收大型
真实模板前，应增加样本并复核这些值。`PackageLimits` 的字段可由底层包 API 调整。

# docxtpl-rs 兼容性清单

> 行为 PR 的单一事实来源之一（代码规范 §1.3）。每项功能登记为 compatible /
> documented-deviation / unsupported，且必须有最小 fixture。
> 状态标记：✔ 差分通过 · ◐ 部分支持/有偏差 · ✘ 不支持 · ⏳ 规划中

## 1. 参考实现指纹（P0 已固定，见 ADR-001）

| 项 | 值 |
|---|---|
| 上游 | python-docx-template（docxtpl）**0.20.2** |
| PyPI | `docxtpl==0.20.2`，2025-11-13 发布 |
| Git | tag `v0.20.2`，SHA `cf5437bdf5d30f9362149ddea508d6d9f008b6cd` |
| 许可证 | LGPL-2.1-only（workspace 声明 LGPL-2.1-or-later，见 ADR-001） |
| 依赖锁 | `tests/oracle/requirements.txt` + `Cargo.lock` |

## 2. 上游渲染管线（审计结论）

审计源：`third_party/docxtpl-0.20.2/docxtpl/template.py`（未入库，见 ADR-001 获取方式）。

1. `get_xml()`：仅序列化 `w:body`（lxml 会把根上可见的 xmlns 声明附加到输出上）。
2. `patch_xml()`：12 个**有界**字符串正则变换（详见 §3）。
3. `render_xml_part()`：每个 `<w:p[ >]` 前插 `\n`（仅为行号定位）→ `jinja2.Template(src).render(context)`
   → 移除 `\n<w:p` → `{_{ }_} {_% %_}` 字面转义还原 → `resolve_listing()`。
4. `render()`（默认 `autoescape=False`）：body → `fix_tables()`（`etree.fromstring(xml,
   parser=XMLParser(recover=True))` 宽松解析 + tblGrid 列数修正）→ `fix_docpr_ids()`
   （body 内 `wp:docPr/@id` 自 1001 顺序重编号）→ 植回 `w:document`。
   随后渲染 headers/footers、core properties（仅 6 个字符串属性、无 patch）、footnotes。
5. 值原样插入 XML；非法 XML 依赖 libxml2 recover 宽松解析愈合（裸 `&` 保留为文本、
   `<b>` 形式注入真实元素），再由 lxml 重序列化（ADR-002）。

## 3. patch_xml 步骤与移植状态（docxtpl-compat）

| # | 语义 | 上游要点 | 阶段/状态 |
|---|---|---|---|
| A | 分隔符拆分愈合 | `{` 与 `{`/`%`/`#` 之间的 XML 标记删除（跨 run/段落均生效） | P2 ✅ |
| B | 标签内跨 run 合并 | 标签内部 `</w:t>…<w:t>` 边界删除 | P2 ✅ |
| C | `{% colspan expr %}` | 承载 `w:tc` 内删空 run、删首个 gridSpan，在 `w:tcPr` 后注入 `w:gridSpan w:val="{{expr}}"` | P3 ✅ |
| D | `{% cellbg expr %}` | 同上，注入 `w:shd w:fill="{{expr}}"` | P3 ✅ |
| E | 含标签的 `w:t` 补 `xml:space="preserve"` | 仅作用于裸 `<w:t>`（无属性） | P2 ✅ |
| F | `{{r …}}` / `{%r …%}` 独立成 run | 前后拆出无格式新 run | P3 ✅ |
| G | `{%(p\|tr\|tc\|r) …%}` / `{{(p\|tr\|tc\|r) …}}` 结构化标签提升 | 带嵌套守卫的正则将**整个承载元素**替换为普通标签；先 tr、tc、p、后 r | P3 ✅ |
| H | `{#(p\|tr\|tc) …#}` 注释提升 | 同 G，内容不含 `}`/`#` | P3 ✅ |
| I | `{% vm %}` 垂直合并 | tcPr 尾部注入 `w:vMerge` restart/continue，文本仅首迭代保留 | P3 ✅ |
| J | `{% hm %}` 水平合并 | gridSpan 值乘 `loop.length`（无则新增），整格仅首迭代保留 | P3 ✅ |
| K | clean_tags | 标签内 `&#8216;`/`&lt;`/`&gt;` 与智能引号（“ ” ‘ ’）还原为 ASCII | P2 ✅ |

## 4. 功能支持矩阵

### P2/P3（MVP，0.1.0-alpha）

| 功能 | 级别 | fixture 类别 |
|---|---|---|
| 普通变量 / 多变量 / 空格容忍 | compatible 目标 | r2_var_* |
| 内置过滤器子集（upper/join/default/…） | compatible 目标 | r2_filter_* |
| undefined 宽松（空串渲染） | compatible 目标 | r2_undefined |
| if/else、for、注释、set | compatible 目标 | r2_if_* / r2_for_* / r2_comment / r3_p_set |
| 空白控制 `{%- -%}` | compatible 目标 | r2_trim |
| 值特殊字符（`&`/`>`/引号/`<`）recover 愈合 | compatible 目标（行为以 oracle 钉死） | r2_value_* |
| 值内 `\n`/`\t`（resolve_listing） | compatible 目标 | r2_newline/tab_value |
| 拆分标签合并（A/B 步） | compatible 目标 | r2_split_* |
| 字面 `{_{ }_}` 转义 | compatible 目标 | r2_literal_escape |
| p/tr/tc/r 结构化标签（含空/单/多循环、嵌套） | compatible 目标 | r3_p_* / r3_tr_* / r3_tc_* |
| colspan / cellbg / vm / hm | compatible 目标 | r3_colspan / cellbg / vm / hm |
| fix_tables 网格增删修正 | compatible 目标 | r3_fix_add / r3_fix_remove |
| fix_docpr_ids 重编号 | compatible 目标 | r3_docpr |
| render_properties 核心属性（6 字符串属性 Jinja 渲染，缺省 dc:identifier/dc:language 元素补齐） | compatible 目标（随每次 render 无条件执行） | 全部 48 个成功 fixture 逐字节覆盖 |
| 渲染错误（语法等）错误类别对齐 | compatible 目标 | r2_syntax_error |

### P4（RichText 与图片，0.1.0-alpha 增量，ADR-005）

| 功能 | 级别 | fixture 类别 |
|---|---|---|
| RichText run 属性（bold/italic/u/strike/color/size/highlight/style/font 区域语法/sup/sub/rtl/lang）与多 run 拼接、空值语义 | compatible 目标 | p4_rt_basic / p4_rt_style_font / p4_rt_in_table |
| RichText 外部超链接（`tpl.build_url_id` 预登记 external rel → `w:hyperlink r:id`） | compatible 目标 | p4_rt_url |
| RichTextParagraph（parastyle 有/无、富文本入段、空段） | compatible 目标 | p4_rtp_basic |
| Listing（`\n \t \a \f` 经 resolve_listing 展开；与 RichText 混排） | compatible 目标 | p4_listing_basic / p4_listing_after_rt / p4_combo_rich |
| InlineImage 原生尺寸（png/jpg/bmp/gif/tiff 头解析、EMU 换算） | compatible 目标 | p4_img_png / p4_img_wh / p4_img_formats |
| InlineImage 单边缩放（纵横比银行家舍入） | compatible 目标 | p4_img_scale_w |
| 图片 sha1 去重（同字节复用 part 与 rId） | compatible 目标 | p4_img_dup / p4_img_in_table / p4_combo_rich |
| 多张不同图片（imageN 编号/rId 空洞回填） | compatible 目标 | p4_img_two / p4_img_formats |
| 图片超链接锚点（图片 rId 先于 anchor external rId） | compatible 目标 | p4_img_anchor |
| 表格行循环内图片（同一值多次解析幂等） | compatible 目标 | p4_img_in_table |
| media part 注入 + document rels 与 [Content_Types].xml 的 python-docx 风格重建 | compatible 目标（逐字节，DEV-0003 不触发） | 全部含图 p4 fixture |
| 坏图片错误类别对齐（UnrecognizedImageError，probe 先于任何 part/rId 分配） | compatible 目标 | p4_img_bad |

### P5–P6（规划）

header/footer 渲染、footnotes 渲染、subdoc 合并。

### unsupported（明确拒绝）

- 传入自定义 `jinja_env` / Jinja2 扩展 / line statements / 任意 Python 对象与可调用。
- `{%p%}` 类标签内容含 `%` 或 `}`（上游正则不匹配，落入普通文本并引发 jinja 语法错误，保持一致报错）。
- DTD / 外部实体（安全侧偏差，ADR-004）。

## 5. documented-deviation 登记

| ID | 描述 | 依据 |
|---|---|---|
| DEV-0001 | 输入限额为上游没有的安全增强；corpus 在限额内，不影响差分 | ADR-004 |
| DEV-0002 | libxml2 recover 的长尾行为仅以 corpus 钉死为限；未钉死输入按保守策略处理并记录诊断 | ADR-002 |
| DEV-0003 | ZIP 时间戳/条目顺序、`[Content_Types].xml` 与 rels 的**条目顺序**视为非语义差异，由 canonicalization 归一化 | 规划文档 §9.5 |
| DEV-0004 | 不复制 python-docx 的包重写行为：未修改 part 尽量原样保留字节（优于上游，语义等价经 c14n 证明） | ADR-002 |
| DEV-0005 | `autoescape=True` 下富内容值（RichText/Listing/InlineImage）的转义口径未与上游对齐：上游经 `__html__` 仍原样注入 XML，本侧 MiniJinja 可能按 HTML 规则转义。P4 全部 fixture 固定 `autoescape=False`，该路径不在 oracle 覆盖内；需开启 autoescape 又渲染富值时不要依赖当前行为 | ADR-005 |

## 6. fixture 基线

见 `tests/fixtures/manifest.json`（P0：20 个往返 + 48 个 P2/P3 渲染用例；
P4 增量：17 个 `p4_*` 渲染用例，共 65 个 render；全部标注
id/feature/phase/mode/context_kind/expected/owner）。差分实测结果见 §7。

## 7. 差分结果（0.1.0-alpha 实测）

执行环境：Windows + rustc/cargo 1.95.0；oracle = Python docxtpl 0.20.2 /
Jinja2 3.1.6 / python-docx 1.2.0 / lxml 6.1.1（见 §1）。
命令：`cargo test -p docxtpl-rs --features oracle --test oracle_diff`。

判定口径（tests/oracle/compare.py）：part 名集合一致 + **每个 part 原始字节
sha256 一致** + `[Content_Types].xml`/rels 集合一致 + 每个 XML part 的
exclusive C14N sha256 一致（docProps/core.xml 时间戳归一化）。

| 类别 | 用例数 | 结果 |
|---|---|---|
| r2_* 渲染（变量/过滤器/undefined/if/for/trim/注释/拆分/字面转义/特殊字符值/listing/智能引号/实体/空循环） | 26 | 26 MATCH |
| r2_syntax_error（jinja2 TemplateSyntaxError） | 1 | 错误类别一致（Syntax） |
| r3_* 渲染（p/tr/tc/r 结构标签、嵌套表、colspan/cellbg/vm/hm、fix_add/remove、docpr、combo） | 22 | 22 MATCH |
| p4_rt_* / p4_rtp_* 渲染（RichText 全属性/超链接、RichTextParagraph、表格单元格富文本） | 5 | 5 MATCH |
| p4_listing_* 渲染（Listing 控制符与富文本混排） | 2 | 2 MATCH |
| p4_img_* 渲染（png/jpg/bmp/gif/tiff、缩放、sha1 去重、多图、锚点、行循环、格式扩展） | 8 | 8 MATCH |
| p4_combo_rich（RichText + Listing + 图片 + 行循环组合） | 1 | 1 MATCH |
| p4_img_bad（UnrecognizedImageError） | 1 | 错误类别一致（Image） |
| rt_* 真实 docx OPC 往返（docxtpl-opc fixture_roundtrip） | 20 | 20 逐 part 字节一致 |
| patch_xml golden（full_patched，docxtpl-compat golden_patch） | 66 | 66 字节一致 |
| stages recover golden（树结构相等，docxtpl-xml golden_recovery） | 64 | 64 一致 |
| **合计** | **216 项断言/用例** | **全部通过，无偏差、无未支持** |

结论：0.1.0-alpha 范围内（§4 P2/P3/P4 矩阵 + render_properties）与 Python
oracle **逐字节等价**（DEV-0003 的条目顺序/时间戳归一化未被触发：实际输出连
原始 part 字节都已一致——含 P4 新增的 media part、document rels 与
[Content_Types].xml 重建）。错误用例的稳定类别（`TemplateErrorKind::Syntax`
/ `TemplateErrorKind::Image`）对齐上游异常分类。

已知边界（不属本阶段验收项）：P5–P6 功能未实现；DEV-0002 的 libxml2 recover
长尾仅以 corpus 与探针规则钉死；DEV-0005 的 autoescape + 富值路径不在
oracle 覆盖内；corpus 之外的输入不承诺字节等价。

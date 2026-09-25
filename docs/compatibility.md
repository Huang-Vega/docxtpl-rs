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
| Subdoc 合并依赖 | docxcompose **2.2.0**（oracle 侧；docxtpl `Subdoc` 经其 `Composer.attach_parts` 合并部件，P6） |
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

### P5（页眉/页脚/脚注，0.1.0-alpha 增量，ADR-006）

| 功能 | 级别 | fixture 类别 |
|---|---|---|
| 页眉/页脚 Jinja 渲染（变量、if、`{%p for%}` 段落循环；多节各自 header/footer；无标签 story 原样往返） | compatible 目标（lxml 往返、resolve_listing 照跑，不做 fix_tables/fix_docpr_ids） | p5_hf_basic / p5_hf_multi / p5_hf_untagged |
| 页眉/页脚 RichText / Listing（外部超链接 build_url_id 作用于主文档 rels） | compatible 目标 | p5_hf_richtext |
| 页眉/页脚 InlineImage（多图、anchor 超链接；rId/media 按 part 作用域分配、sha1 包级去重；docPr part 级常数 id/name；story 紧凑序列化与冗余 xmlns 保留） | compatible 目标（逐字节） | p5_hf_image |
| 脚注 part 字符串渲染（保留模板 XML 声明与未改字节；RichText/Listing 同管线） | compatible 目标 | p5_footnotes_basic |
| story 语法错误带具体 part 名（错误类别对齐 TemplateSyntaxError） | compatible 目标 | p5_hf_syntax_error |
| `[Content_Types].xml` 渲染后归一（Default 按扩展名、Override 按 part 名排序） | compatible 目标 | p5_footnotes_basic 实证 |

### P6（Subdoc 子文档合并，0.1.0-alpha 增量，ADR-007）

| 功能 | 级别 | fixture 类别 |
|---|---|---|
| `new_subdoc(docpath)` 外部 docx 片段注入（`{{p sd }}` 构造期合并；片段 jinja 单遍求值、字面标签文本原样输出；空 body 片段） | compatible 目标（逐字节） | p6_subdoc_basic / p6_subdoc_verbatim / p6_subdoc_untagged |
| 样式合并三分支（sub id→name→主 id 映射复用；主缺样式 deepcopy append + 编号/linked styles 链；引用 fall-through 改写；styles.xml dirty 门控） | compatible 目标（逐字节） | p6_subdoc_style |
| 子文档图片合并（扩展名取源 part 后缀、CT 取源包声明、字节 sha1 去重；media/主 rels/CT 经 ImageInjections 随 finish 落定）与 external 超链接关系迁移 | compatible 目标（逐字节） | p6_subdoc_image |
| 引用部件递归复制（partname/rId 空洞回填）、bookmark/docPr/cNvPr 重编号、页眉页脚引用剥离、分节守卫（语料内均走 no-op 路径，由字节基线钉死） | compatible 目标 | 全部 p6 fixture |

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
| DEV-0005 | `autoescape=True` 下富内容值（RichText/Listing/InlineImage）的转义口径未与上游对齐：上游经 `__html__` 仍原样注入 XML，本侧 MiniJinja 可能按 HTML 规则转义。P4/P5 全部 fixture 固定 `autoescape=False`，该路径不在 oracle 覆盖内；需开启 autoescape 又渲染富值时不要依赖当前行为 | ADR-005 |
| DEV-0006 | 脚注（及任何非 story 通用 Part）中使用 InlineImage 不支持：上游在未注册 PartFactory 的二进制 Part 上调用 `new_pic_inline` 会抛 `AttributeError`；本侧 `render_footnotes_xml_ctx` 以 `NullRegistry` 在占位符解析阶段返回带 part 名的错误。P5 fixture 未覆盖该错误路径（由单测钉死） | ADR-006 |
| DEV-0007 | 同一 part 被主文档 rels 中多条 header/footer 关系引用时只渲染一次（按目标去重）；endnotes（`word/endnotes.xml`）不在 P5 范围；story part 枚举仅扫描主文档 rels 的 Internal 目标，外部/孤立 story 不渲染 | ADR-006 |
| DEV-0008 | Subdoc 仅支持外部 docx 路径模式（`RenderSession::new_subdoc(docpath)`）；上游无 docpath 的借用模式（`Subdoc` 直接复用当前文档 part）API 不提供；Subdoc 值不能经 JSON 上下文传入 | ADR-007 |
| DEV-0009 | 子文档含自定义属性部件（`docProps/custom.xml`）时，上游 `dissolve_fields` 把域并入主包核心属性；本侧在合并开始时即返回带 part 名的 Malformed 错误，P6 fixture 全域规避 | ADR-007 |
| DEV-0010 | 子文档编号合并的非确定性/隐式建部件路径不支持，检测到即返回带 part 名错误：复制的 `w:abstractNum` 含 `w:nsid`（上游按 `random.random()` 重写，输出非确定）；主包缺 `word/numbering.xml` 而子文档引用编号（上游从内置默认模板新建该 part）；`restart_first_numbering` 穿过全部 guard 后实际触发编号重启修改块。guard 链正常退出（标题 outlineLvl / bullet / 无 pStyle / 无 numId）仍为 no-op，与上游一致 | ADR-007 |
| DEV-0011 | 子文档 SmartArt（`dgm:relIds[@r:dm]`）、VML 形状图片（`v:shape`/`v:imagedata`）、脚注引用（`w:footnoteReference`）的部件合并不支持，检测到即返回错误；P6 fixture 全域规避 | ADR-007 |
| DEV-0012 | 主文档与子文档均含多个分节时，`fix_section_types` 需改写主分节起始类型，本侧返回错误；任一侧 section≤1 时上游本就 no-op，行为一致 | ADR-007 |

## 6. fixture 基线

见 `tests/fixtures/manifest.json`（P0：20 个往返；P2/P3：48 个成功 +
1 个 error 预期 = 49 个；P4：16 个成功 + 1 个 error 预期 = 17 个；
P5：6 个成功 + 1 个 error 预期 = 7 个；P6：5 个成功 = 5 个
（另各带 `templates/<id>_sub.docx` 子文档）；共 78 个 `mode=render`
条目；全部标注 id/feature/phase/mode/context_kind/expected/owner）。
差分实测结果见 §7。

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
| p5_hf_basic / p5_hf_multi / p5_hf_untagged（页眉页脚变量/if/段落循环、多节、无标签往返） | 3 | 3 MATCH |
| p5_hf_richtext（页眉 RichText/Listing + 正文 RichText 外链） | 1 | 1 MATCH |
| p5_hf_image（页眉 2 图含 anchor、页脚 1 图、正文 1 图，多 owner rels/sha1 共享） | 1 | 1 MATCH |
| p5_footnotes_basic（脚注变量/RichText/Listing + CT 排序归一） | 1 | 1 MATCH |
| p5_hf_syntax_error（页眉 TemplateSyntaxError） | 1 | 错误类别一致（Syntax，part=word/header1.xml） |
| p6_subdoc_basic / p6_subdoc_verbatim / p6_subdoc_untagged（子文档普通段落片段、字面 jinja 标签不二次求值、空 body 片段） | 3 | 3 MATCH |
| p6_subdoc_style（样式 name 映射复用 + 自定义样式 deepcopy append + 编号/linked 链） | 1 | 1 MATCH |
| p6_subdoc_image（子文档图片字节 sha1 合并、media/主 rels/CT 落定 + external 超链接关系迁移） | 1 | 1 MATCH |
| rt_* 真实 docx OPC 往返（docxtpl-opc fixture_roundtrip） | 20 | 20 逐 part 字节一致 |
| patch_xml golden（full_patched，docxtpl-compat golden_patch） | 78 | 78 字节一致 |
| stages recover golden（树结构相等，docxtpl-xml golden_recovery） | 76 | 76 一致 |
| **合计** | **252 项断言/用例** | **全部通过，无偏差、无未支持** |

结论：0.1.0-alpha 范围内（§4 P2/P3/P4/P5/P6 矩阵 + render_properties）与
Python oracle **逐字节等价**（DEV-0003 的条目顺序/时间戳归一化未被触发：
实际输出连原始 part 字节都已一致——含 P4 新增的 media part、document rels
与 [Content_Types].xml 重建，P5 多 owner story rels 与 CT 排序归一，以及
P6 子文档合并写入的样式/图片 part、主 rels 与 CT 变更）。
错误用例的稳定类别（`TemplateErrorKind::Syntax` /
`TemplateErrorKind::Image`）对齐上游异常分类，story/subdoc 错误带具体
part 名。

已知边界（不属本阶段验收项）：DEV-0006 脚注图片、DEV-0007 同 part 双
rel/endnotes/外部 story 不支持；DEV-0008～DEV-0012 的 Subdoc 借用模式/
custom.xml/编号非确定路径/SmartArt/VML/脚注/两侧多节不支持；DEV-0002 的
libxml2 recover 长尾仅以 corpus 与探针规则钉死；DEV-0005 的 autoescape +
富值路径不在 oracle 覆盖内；corpus 之外的输入不承诺字节等价。

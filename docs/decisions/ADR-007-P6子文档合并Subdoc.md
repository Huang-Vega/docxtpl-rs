# ADR-007：P6 子文档合并（Subdoc / new_subdoc）

日期：2026-09-25
状态：已接受
阶段：P6（Subdoc 子文档合并）

## 背景

P0–P5 覆盖单个模板自身的全部可变 part。上游 docxtpl 另提供
`tpl.new_subdoc(docpath)`：在**构造上下文时**把一个外部 docx 的部件
（样式、编号、关系、media、Content Types）合并进主文档包，body 内容
则作为 XML 片段值经模板位 `{{p sd }}` 注入。该能力由
docxtpl 0.20.2 的 `Subdoc` 调用 docxcompose 2.2.0 的 `Composer`
（`attach_parts`）实现。本 ADR 记录 P6 的合并编排、落盘策略与
非确定性语义的拒绝边界。

## 探针钉死的上游事实（docxtpl 0.20.2 + docxcompose 2.2.0 + python-docx 1.2.0 + lxml 6.1.1）

1. **模板位是表达式 `{{p sd }}`，不是语句标签**：patch_xml G 步把承载
   整段 `w:p` 提升为裸 `{{ sd }}`，渲染路径与 RichTextParagraph
   （P4）完全同构；`Subdoc.__html__`/`__str__` = `_get_xml()` =
   `etree.tostring(body)` 后正则剥掉 `<w:body>` 开闭标签，提升到
   body 标签上的 xmlns 声明随标签一并丢失，片段依赖主文档根声明兜底。
2. **jinja 单遍求值**：片段内的 `{% x %}`/`{{ y }}` 文本原样出现在
   渲染结果中，不会被二次求值（p6_subdoc_verbatim 钉死）。
3. **构造期合并**：`Subdoc(tpl, docpath)` 构造时立即执行
   `attach_parts`，全部 part 变更在渲染前持久写进主包；body 元素
   **无 deepcopy**（直接改 sub 自己的树），无 fix_header_and_footers。
4. **attach_parts 固定顺序**（docxtpl/subdoc.py）：
   custom properties `dissolve_fields` → 建样式 id/name 映射 →
   逐 sub body 直接子级（跳过 `w:sectPr`）：add_referenced_parts →
   add_styles → add_numberings → restart_first_numbering →
   add_images → add_diagrams → add_shapes → add_footnotes →
   remove_header_and_footer_references；循环后
   add_styles_from_other_parts → renumber_bookmarks →
   renumber_docpr_ids → renumber_nvpicpr_ids → fix_section_types。
5. **add_referenced_parts**：`.//*[@r:id]` 按文档序；IMAGE/
   HEADER/FOOTER reltype 跳过（悬空保留，分别由 add_images 与
   remove_header_and_footer_references 处理）；external →
   主 rels get_or_add（reltype+target+mode 幂等）；internal →
   `copy_part` 递归复制整棵 part/rels 图，partname 按
   `FILENAME_IDX_RE = ([a-zA-Z/_-]+)([1-9][0-9]*)?` 取前缀重新编号、
   回填空洞，源 rels 按 rId 数字序复制（副本 rId 连续时与源一致）。
6. **样式三分支**：used_style_ids 经 OrderedDict.fromkeys 保序去重
   （tblStyle/pStyle/rStyle 的 w:val）；sub 样式 id → w:name →
   主样式 id 映射。主样式元素全等→保留；主缺→deepcopy 副本 append
   并追加其编号与 linked styles；主有不同 id→沿
   numbering↔linked 链 fall-through 改写引用（各环缺失为 no-op，
   **不能 early continue**，否则漏掉尾段引用改写）。
7. **编号合并**：`_next_numbering_ids` 在循环前取一次——w:num 的
   numId max+1（无则 1），w:abstractNum 的 abstractNumId max+1
   （无则 0）；`_insert_num` 实际插在**最后一个 w:num 之前**
   （上游源码注释 "after" 与代码相反），`_insert_abstract_num`
   插在第一个 num 前（无 num 则 insert(0)）；sub 缺对应
   abstractNum 时 continue（mapping 残留、num 不插、收尾的悬空
   numId 改写仍照走）；abstractNum 含 `w:nsid` 时上游用
   `random.random()` 重写 nsid——**非确定性输出**。
8. **restart_first_numbering 恒以 restart=True 调用**
   （docxcompose 2.2.0；早期计划文档所写 "False" 系讹误）。guard 链
   任一命中即正常退出：标题样式 outlineLvl、bullet numFmt、无
   pStyle、样式/正文无 numId；真正进入修改块才执行编号重启。
   主包缺 word/numbering.xml 时，上游从内置默认模板新建该 part。
9. **add_images**：`(.//a:blip|.//asvg:svgBlip)[@r:embed]` 文档序；
   ImageWrapper 的扩展名取**源 part 文件名后缀**、content type 取
   **源包 [Content_Types].xml 的声明**（非字节头探测；早期计划文档
   所写 "字节探测" 系讹误），sha1 按字节；命中复用 partname/rId，
   未命中 `word/media/imageN.ext` 跨扩展名回填空洞；
   `r:link` external 走 add_relationship。
10. **renumber 三兄弟作用于主文档**：bookmarkStart 与 bookmarkEnd
    各自独立从 0 计数；wp:docPr、pic:cNvPr 各自从 1，body 之后按
    主 rels 中 HEADER/FOOTER 目标（插入序、不去重）续同一计数器；
    body 内 docPr 随后被渲染期 fix_docpr_ids 重排为 1001 起，
    hf part 内的编号持续有效（P5 story 渲染不做 fix_docpr_ids）。
11. **fix_section_types**：任一侧 section≤1（body 直接 sectPr +
    pPr 内 sectPr 计数）即 no-op；两侧均多节时需改写主分节起始
    类型。
12. **python-docx 全量保存**：docxtpl save 经 iter_parts 图遍历由
    PackageWriter 重建 CT/rels，compose 期全部树改动随之落盘。
    `_ContentTypesItem._add_part`（register_content_type）：同扩展名
    已有 Default 且 CT 相同→不动；同扩展名异 CT→add_override；
    扩展名无 Default→add_default。

## 决策

1. **值类型**（docxtpl-template）：新增 `RenderValue::Subdoc(String)`
   持预生成片段；`value_to_minijinja` 以 `Value::from_safe_string`
   输出（对齐 `Subdoc.__html__`，autoescape 开/关均原样注入，
   DEV-0005 的富值 autoescape 偏差不涉及 Subdoc）；不提供
   `From<String>`（避免与既有 `From<String>→Json` 冲突）；JSON
   上下文路径不支持（同 InlineImage）。
2. **docxtpl-xml 能力补齐**：新增 `serialize_subtree`（从指定节点
   输出、无 XML 声明、无额外 ns 声明的子树序列化）、
   `insert_child_at`（对齐 lxml `element.insert(index, child)`）、
   `deepcopy_element`（跨文档元素深拷贝）；主树既有序列化路径
   零改动，golden 零回归。
3. **新模块 `docxtpl-rs/src/subdoc.rs`**：`SubdocComposer` 按上游
   attach_parts 1:1 编排；全部 XML part 以 `parse_strict` +
   `strip_blank_text()` 载入（对齐 python-docx oxml 解析器的
   remove_blank_text=True）。
4. **落盘策略**：主 document/styles/numbering 与页眉页脚树 part
   dirty 门控，未改字节原样保留（DEV-0004 原则），改则按
   python-docx 形态序列化（剥空白树 + lxml 单引号声明）；复制的
   非图片 part 与其 rels、Content Types 变更即时落包
   （`register_content_type` 复刻 `_add_part` 三分支）；图片 part
   与主文档 rels 走 `ImageInjections` 暂存（方案 X：合并期同步
   used_numbers/by_sha1/known_defaults/owners[0] pending，
   `RenderSession::finish` 统一落定，杜绝渲染期撞号或重复 push）。
5. **docxtpl-opc**：新增 `ContentTypes::add_override`（Override
   保序尾插，ASCII 排序只发生在 to_xml）。
6. **门面 API**：`RenderSession::new_subdoc(path) ->
   Result<RenderValue, Error>`，构造期合并、同一会话可多次调用；
   畸形 sub 包（缺 rels/悬空 part/缺 CT/rel 缺失或 external、
   num 缺 abstractNumId 等上游会抛 KeyError/IndexError 的输入）
   统一返回带 part 名的 `Malformed` 错误。
7. **非确定性与未覆盖语义保守拒绝**（DEV-0008～DEV-0012）：
   无 docpath 借用模式不提供；custom.xml dissolve_fields 拒绝；
   abstractNum 含 nsid、主缺 numbering.xml 的编号合并、
   restart_first_numbering 修改块实际触发拒绝；SmartArt/VML/脚注
   引用检测即拒绝；两侧均多节拒绝。P6 fixture 全域规避这些路径。
8. **fixture**：新增 5 个 `p6_*`（basic/style/image/verbatim/
   untagged，全部 context_kind="python"），sub docx 存
   `templates/<id>_sub.docx`；全域规避 bookmark/图片（主模板侧）/
   多 section/w:numId/脚注/dgm/VML/custom props，使 renumber
   三兄弟与 fix_section_types 在主文档侧走 no-op；oracle 侧 runner
   真实运行 docxtpl + docxcompose 2.2.0。不得改 fixture/golden
   放宽断言。

## 影响

- 新公开 API：`RenderSession::new_subdoc`（RenderValue::Subdoc
  随 render_ctx 类型 re-export）；`render`/`render_ctx`/`finish`
  既有行为不变。
- 验收：78 个 render fixture 全部与 Python oracle 一致（75 逐 part
  字节匹配 + 3 错误类别匹配），其中 5 个 p6 全部字节 MATCH
  （styles.xml 在仅复用既有样式时保持原字节；含图 fixture 的
  media/主 rels/CT 由 finish 一次性落定）；golden 计数 78
  （full_patched）/76（pre_recover）。
- 已知限制：DEV-0008～DEV-0012（compatibility.md §5）。

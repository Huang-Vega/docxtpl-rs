# Microsoft Word COM validation — 2026-10-01

## Environment

- Product: Microsoft Office LTSC Professional Plus 2024
- Word version: 16.0
- Word build: 16.0.17932
- Platform: x64 Windows
- Automation: background `Word.Application` COM instance
- Macro policy: `msoAutomationSecurityForceDisable` (`3`)
- Alerts: disabled for unattended validation
- Save format: `wdFormatDocumentDefault` (`16`, DOCX)

## Method

Each source document was opened from `tests/oracle/expected`, saved to a new
project-local temporary path, closed, and reopened through Word COM. The check
compared Word's paragraph, table, inline-shape, shape, section, comment,
footnote, header, and footer counts before and after the save. Original fixtures
were never overwritten.

## Results

| Sample | Coverage | Result |
| --- | --- | --- |
| `r2_var_basic.docx` | basic substitution | PASS |
| `r3_tr_for.docx` | table-row loop | PASS |
| `r3_tc_for.docx` | table-cell loop | PASS |
| `r3_vm.docx` | vertical merge | PASS |
| `r3_hm.docx` | horizontal merge | PASS |
| `r3_nested_tables.docx` | nested tables | PASS |
| `r3_combo_invoice.docx` | combined invoice table | PASS |
| `p4_combo_rich.docx` | rich text and inline images | PASS |
| `p5_hf_image.docx` | header/footer image | PASS |
| `p7b_footnotes_real.docx` | real-Word footnote | PASS |
| `p7b_comments.docx` | real-Word comments corpus case | PASS |
| `p7b_word2016.docx` | Word 2016 compatibility corpus | PASS |

All 12 documents opened, saved, and reopened successfully. All recorded
structure counters were unchanged.

The synthetic `p5_footnotes_basic.docx` fixture was not used as the Office
footnote gate because Word reports that generated fixture as possibly corrupt.
It remains an oracle/package normalization fixture. The Office gate instead
uses `p7b_footnotes_real.docx`, whose source is a real Word document and whose
single footnote remained present after the COM save/reopen cycle.

// This suite intentionally keeps coverage for the deprecated compatibility
// entry points alongside their replacements.
#![allow(deprecated)]

use std::io::Cursor;
use std::sync::Arc;

use docxtpl_opc::TargetMode;
use docxtpl_rs::{
    Bookmark, CancellationError, CancellationToken, DefaultFragmentResourceResolver, DocxTemplate,
    EditableStoryKind, EditableStorySelection, FailurePolicy, FilePartSource, FormattingPolicy,
    FragmentDocument, FragmentImportLimits, FragmentImportOptions, FragmentImportSettings,
    FragmentInsertion, FragmentRelationshipView, FragmentResourceDecision,
    FragmentResourceResolver, FragmentResourceSource, ImageLayout, InlineImageOptions, MediaSource,
    Package, PackageLimits, PartCachePolicy, PostprocessError, PostprocessRunError,
    ProbedMediaFile, RenderOptions, RenderedDocument, ResourceLimits, RunFormatOverrides,
    RunTextLimits, StoryKind, StoryResources, StoryScope, WordFragment,
};
use serde_json::json;

const TEMPLATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates/r2_var_basic.docx"
);
const STORY_TEMPLATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates/p5_hf_basic.docx"
);
const MEDIA: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/media/p4_dot2x1.png"
);
const DRAWING_TEMPLATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates/r3_docpr.docx"
);
const NOTE_TEMPLATE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/fixtures/templates/p7b_footnotes_real.docx"
);

#[derive(Default)]
struct FileBackedImageResolver {
    image_calls: usize,
}

impl FragmentResourceResolver for FileBackedImageResolver {
    fn resolve(
        &mut self,
        relationship: &FragmentRelationshipView,
        _source: &FragmentResourceSource<'_>,
        resources: &mut StoryResources<'_, '_, '_>,
    ) -> Result<FragmentResourceDecision, docxtpl_rs::Error> {
        if relationship.relationship_type.ends_with("/image") {
            self.image_calls += 1;
            return Ok(FragmentResourceDecision::UseMedia(
                resources.register_media_path(MEDIA)?,
            ));
        }
        Ok(FragmentResourceDecision::ImportDefault)
    }
}

fn import_simple_fragment_into_selection(
    document: &mut RenderedDocument,
    selection: EditableStorySelection,
    source_bytes: &[u8],
) -> Result<Vec<EditableStoryKind>, docxtpl_rs::Error> {
    let mut visited = Vec::new();
    document.postprocess(|pipeline| {
        pipeline.pass("six-story-fragment", FailurePolicy::Abort, |transaction| {
            transaction.for_each_editable_story_with_resources(selection, |context| {
                let source =
                    Package::from_reader(Cursor::new(source_bytes), &PackageLimits::default())?;
                let fragment = WordFragment::from_package(source, "word/document.xml")?;
                let paragraph =
                    context
                        .story()
                        .document()
                        .descendants(context.story().document().root())
                        .into_iter()
                        .find(|node| {
                            context.story().document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p"
                            })
                        })
                        .expect("selected Story has a paragraph");
                context.import_fragment(paragraph, fragment, FragmentImportOptions::default())?;
                visited.push(context.story().kind());
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    })?;

    Ok(visited)
}

#[test]
fn fragment_document_bytes_api_replaces_a_target_and_reports_nodes(
) -> Result<(), Box<dyn std::error::Error>> {
    let source_template = DocxTemplate::open(TEMPLATE)?;
    let source = source_template.render(
        &json!({"name": "fragment document source"}),
        &RenderOptions::compat(),
    )?;
    let source_bytes: Arc<[u8]> = Arc::from(source.to_bytes()?);
    let fragment_document =
        FragmentDocument::from_docx_bytes(source_bytes, &FragmentImportLimits::default())?;
    let mut fragment = fragment_document.story("word/document.xml")?;
    let roots = fragment.body_children();
    assert!(!roots.is_empty());
    assert!(fragment.tag(roots[0]).is_some_and(|tag| {
        tag.ns == docxtpl_xml::ns_uri::W && matches!(tag.local.as_str(), "p" | "tbl")
    }));
    fragment.select_roots([roots[0]])?;

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut target = template.render(
        &json!({"name": "fragment target removed"}),
        &RenderOptions::compat(),
    )?;
    let mut fragment = Some(fragment);
    let mut detailed = None;
    target.postprocess(|pipeline| {
        pipeline.pass("fragment-replace", FailurePolicy::Abort, |transaction| {
            transaction.for_each_editable_story_with_resources(
                EditableStorySelection::BODY,
                |context| {
                    let target_node = context
                        .story()
                        .document()
                        .descendants(context.story().document().root())
                        .into_iter()
                        .find(|node| {
                            context.story().document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p"
                            })
                        })
                        .expect("target paragraph");
                    detailed = Some(context.import_fragment_with_resolver(
                        fragment.take().expect("body visited once"),
                        FragmentInsertion::Replace {
                            target: target_node,
                        },
                        &FragmentImportSettings::default(),
                        &mut DefaultFragmentResourceResolver,
                    )?);
                    Ok(())
                },
            )?;
            Ok(())
        })?;
        Ok(())
    })?;

    let detailed = detailed.expect("detailed fragment report");
    assert_eq!(detailed.summary.inserted_nodes, 1);
    assert!(detailed.imported_total_nodes >= 1);
    assert!(detailed.relationship_map.is_empty());
    let package = Package::from_reader(Cursor::new(target.to_bytes()?), &PackageLimits::default())?;
    package.validate()?;
    let xml = std::str::from_utf8(package.part("word/document.xml").unwrap().bytes()?)?;
    assert!(xml.contains("fragment document source"));
    assert!(!xml.contains("fragment target removed"));
    Ok(())
}

#[test]
fn fragment_document_rejects_compressed_source_over_limit() -> Result<(), Box<dyn std::error::Error>>
{
    let source_template = DocxTemplate::open(TEMPLATE)?;
    let source = source_template.render(&json!({"name": "limit"}), &RenderOptions::compat())?;
    let source_bytes: Arc<[u8]> = Arc::from(source.to_bytes()?);
    let mut limits = FragmentImportLimits::default();
    limits.max_source_docx_bytes = source_bytes.len() as u64 - 1;
    let error = FragmentDocument::from_docx_bytes(source_bytes, &limits)
        .err()
        .expect("oversized source must fail");
    assert!(error.to_string().contains("fragment_source_docx"));
    Ok(())
}

#[test]
fn fragment_resolver_replaces_source_image_with_file_backed_media(
) -> Result<(), Box<dyn std::error::Error>> {
    let source_bytes: Arc<[u8]> = Arc::from(std::fs::read(DRAWING_TEMPLATE)?);
    let fragment_document =
        FragmentDocument::from_docx_bytes(source_bytes, &FragmentImportLimits::default())?;
    let fragment = fragment_document.story("word/document.xml")?;

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut target = template.render(
        &json!({"name": "resolver target"}),
        &RenderOptions::compat(),
    )?;
    let mut fragment = Some(fragment);
    let mut resolver = FileBackedImageResolver::default();
    let mut detailed = None;
    target.postprocess(|pipeline| {
        pipeline.pass("fragment-resolver", FailurePolicy::Abort, |transaction| {
            transaction.for_each_editable_story_with_resources(
                EditableStorySelection::BODY,
                |context| {
                    let anchor = context
                        .story()
                        .document()
                        .descendants(context.story().document().root())
                        .into_iter()
                        .find(|node| {
                            context.story().document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p"
                            })
                        })
                        .expect("target paragraph");
                    detailed = Some(context.import_fragment_with_resolver(
                        fragment.take().expect("body visited once"),
                        FragmentInsertion::Before { anchor },
                        &FragmentImportSettings::default(),
                        &mut resolver,
                    )?);
                    Ok(())
                },
            )?;
            Ok(())
        })?;
        Ok(())
    })?;

    let detailed = detailed.expect("detailed fragment report");
    assert!(resolver.image_calls >= 1);
    assert!(detailed.summary.image_relationships >= 1);
    assert_eq!(detailed.media_bytes_read, 0);
    assert!(detailed
        .relationship_map
        .iter()
        .any(|mapping| mapping.target_id.is_some()));
    let package = Package::from_reader(Cursor::new(target.to_bytes()?), &PackageLimits::default())?;
    package.validate()?;
    Ok(())
}

#[test]
fn unified_story_editor_visits_and_edits_notes() -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(NOTE_TEMPLATE)?;
    let mut document = template.render(
        &json!({"a_jinja_variable": "A Jinja variable!"}),
        &RenderOptions::compat(),
    )?;
    let mut visited = Vec::new();
    let mut story_report = None;

    document.postprocess(|pipeline| {
        pipeline.pass("edit-footnote", FailurePolicy::Abort, |transaction| {
            story_report = Some(transaction.for_each_editable_story(
                EditableStorySelection::ALL,
                |story| {
                    visited.push((story.name().to_string(), story.kind()));
                    if story.kind() != EditableStoryKind::Footnote {
                        return Ok(());
                    }
                    let paragraphs: Vec<_> = story
                        .document()
                        .descendants(story.document().root())
                        .into_iter()
                        .filter(|node| {
                            story.document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p"
                            })
                        })
                        .collect();
                    for paragraph in paragraphs {
                        let index = story.run_text_index(paragraph, RunTextLimits::default())?;
                        if let Some(matched) =
                            index.find_literal("A Jinja variable!")?.into_iter().next()
                        {
                            story.replace_text_match(
                                &index,
                                &matched,
                                "edited footnote",
                                FormattingPolicy::InheritFirstRun,
                            )?;
                        }
                    }
                    Ok(())
                },
            )?);
            Ok(())
        })?;
        Ok(())
    })?;

    assert_eq!(
        visited.first().map(|(_, kind)| *kind),
        Some(EditableStoryKind::Body)
    );
    assert!(visited
        .iter()
        .any(|(_, kind)| *kind == EditableStoryKind::Footnote));
    assert!(visited
        .iter()
        .any(|(_, kind)| *kind == EditableStoryKind::Endnote));
    let footnote_position = visited
        .iter()
        .position(|(_, kind)| *kind == EditableStoryKind::Footnote)
        .expect("footnote story");
    let endnote_position = visited
        .iter()
        .position(|(_, kind)| *kind == EditableStoryKind::Endnote)
        .expect("endnote story");
    assert!(footnote_position < endnote_position);
    let report = story_report.expect("unified story report");
    assert!(report
        .changed_parts
        .contains(&"word/footnotes.xml".to_string()));

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let footnotes = std::str::from_utf8(reopened.part("word/footnotes.xml").unwrap().bytes()?)?;
    assert!(footnotes.contains("edited footnote"));
    assert!(!footnotes.contains("A Jinja variable!"));
    Ok(())
}

#[test]
fn story_resource_context_uses_current_owner_for_body_headers_footers_and_notes(
) -> Result<(), Box<dyn std::error::Error>> {
    let media_bytes: Arc<[u8]> = Arc::from(std::fs::read(MEDIA)?);
    let context: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/contexts/p5_hf_basic.json"
    )))?;
    let template = DocxTemplate::open(STORY_TEMPLATE)?;
    let mut document = template.render(&context, &RenderOptions::compat())?;
    let mut visited = Vec::new();

    document.postprocess(|pipeline| {
        pipeline.pass("story-resources", FailurePolicy::Abort, |transaction| {
            let report = transaction.for_each_editable_story_with_resources(
                EditableStorySelection::BODY_HEADERS_FOOTERS,
                |story| {
                    let owner = story.story().name().to_string();
                    let kind = story.story().kind();
                    let media = story
                        .resources()
                        .register_media_bytes("photo.bin", Arc::clone(&media_bytes))?;
                    let rid = story.resources().relate_image(&media)?;
                    visited.push((owner, kind, rid));
                    Ok(())
                },
            )?;
            assert_eq!(report.parsed_parts, visited.len());
            assert_eq!(report.serialized_parts, 0);
            Ok(())
        })?;
        Ok(())
    })?;

    assert!(visited
        .iter()
        .any(|(_, kind, _)| *kind == EditableStoryKind::Body));
    assert!(visited
        .iter()
        .any(|(_, kind, _)| *kind == EditableStoryKind::Header));
    assert!(visited
        .iter()
        .any(|(_, kind, _)| *kind == EditableStoryKind::Footer));
    let reopened =
        Package::from_reader(Cursor::new(document.to_bytes()?), &PackageLimits::default())?;
    for (owner, _, rid) in visited {
        assert!(reopened
            .relationships_of(&owner)
            .is_some_and(|rels| rels.get(&rid).is_some()));
    }

    let template = DocxTemplate::open(NOTE_TEMPLATE)?;
    let mut document = template.render(
        &json!({"a_jinja_variable": "A Jinja variable!"}),
        &RenderOptions::compat(),
    )?;
    let mut note_kinds = Vec::new();
    document.postprocess(|pipeline| {
        pipeline.pass("note-resources", FailurePolicy::Abort, |transaction| {
            transaction.for_each_editable_story_with_resources(
                EditableStorySelection::NOTES,
                |story| {
                    let media = story
                        .resources()
                        .register_media_bytes("note.png", Arc::clone(&media_bytes))?;
                    story.resources().relate_image(&media)?;
                    note_kinds.push(story.story().kind());
                    Ok(())
                },
            )?;
            Ok(())
        })?;
        Ok(())
    })?;
    assert!(note_kinds.contains(&EditableStoryKind::Footnote));
    assert!(note_kinds.contains(&EditableStoryKind::Endnote));
    document.to_bytes()?;
    Ok(())
}

#[test]
fn inline_image_insertion_reuses_relationships_and_clone_renumbers_drawing_ids(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document =
        template.render(&json!({"name": "drawing target"}), &RenderOptions::compat())?;
    let media_bytes: Arc<[u8]> = Arc::from(std::fs::read(MEDIA)?);

    document.postprocess(|pipeline| {
        pipeline.pass(
            "insert-inline-images",
            FailurePolicy::Abort,
            |transaction| {
                transaction.for_each_editable_story_with_resources(
                    EditableStorySelection::BODY,
                    |context| {
                        let media = context
                            .resources()
                            .register_media_bytes("photo.png", Arc::clone(&media_bytes))?;
                        let target_run = context
                            .story()
                            .document()
                            .descendants(context.story().document().root())
                            .into_iter()
                            .find(|node| {
                                context.story().document().tag(*node).is_some_and(|tag| {
                                    tag.ns == docxtpl_xml::ns_uri::W && tag.local == "r"
                                })
                            })
                            .expect("body run");
                        let options = InlineImageOptions {
                            layout: ImageLayout::FitWithin {
                                width: 100_000,
                                height: 100_000,
                            },
                            title: Some("Product photo".to_string()),
                            description: Some("Accessible description".to_string()),
                            hyperlink: Some("https://example.test/product".to_string()),
                        };
                        let first = context.insert_inline_image(target_run, &media, &options)?;
                        let second = context.insert_inline_image(target_run, &media, &options)?;
                        assert_eq!(first.relationship_id, second.relationship_id);
                        assert_ne!(first.doc_pr_id, second.doc_pr_id);
                        assert_ne!(first.picture_id, second.picture_id);
                        context.clone_drawing_to_run(first.drawing, target_run)?;
                        Ok(())
                    },
                )?;
                Ok(())
            },
        )?;
        Ok(())
    })?;

    let reopened =
        Package::from_reader(Cursor::new(document.to_bytes()?), &PackageLimits::default())?;
    reopened.validate()?;
    let relationships = reopened
        .relationships_of("word/document.xml")
        .expect("document relationships");
    assert_eq!(
        relationships
            .iter()
            .filter(|relationship| relationship.rel_type.ends_with("/image"))
            .count(),
        1
    );
    assert_eq!(
        relationships
            .iter()
            .filter(|relationship| relationship.target == "https://example.test/product")
            .count(),
        1
    );
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert_eq!(xml.matches("<w:drawing").count(), 3);
    assert_eq!(xml.matches(r#"cx="100000" cy="50000""#).count(), 6);
    assert_eq!(xml.matches(r#"title="Product photo""#).count(), 6);
    assert_eq!(xml.matches(r#"descr="Accessible description""#).count(), 6);

    let parsed = docxtpl_xml::XmlDocument::parse_strict(xml, &docxtpl_xml::XmlLimits::default())?;
    for (namespace, local) in [
        (docxtpl_xml::ns_uri::WP, "docPr"),
        (docxtpl_xml::ns_uri::PIC, "cNvPr"),
    ] {
        let ids: Vec<_> = parsed
            .descendants(parsed.root())
            .into_iter()
            .filter(|node| {
                parsed
                    .tag(*node)
                    .is_some_and(|tag| tag.ns == namespace && tag.local == local)
            })
            .map(|node| parsed.attr(node, "", "id").unwrap().to_string())
            .collect();
        assert_eq!(ids.len(), 3);
        assert_eq!(
            ids.iter().collect::<std::collections::HashSet<_>>().len(),
            3
        );
    }
    Ok(())
}

#[test]
fn duplicate_managed_drawing_id_rolls_back_the_shared_transaction(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document =
        template.render(&json!({"name": "drawing target"}), &RenderOptions::compat())?;
    let media_bytes: Arc<[u8]> = Arc::from(std::fs::read(MEDIA)?);

    let report = document.postprocess(|pipeline| {
        pipeline.pass(
            "duplicate-drawing-id",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                transaction.for_each_editable_story_with_resources(
                    EditableStorySelection::BODY,
                    |context| {
                        let media = context
                            .resources()
                            .register_media_bytes("photo.png", Arc::clone(&media_bytes))?;
                        let target_run = context
                            .story()
                            .document()
                            .descendants(context.story().document().root())
                            .into_iter()
                            .find(|node| {
                                context.story().document().tag(*node).is_some_and(|tag| {
                                    tag.ns == docxtpl_xml::ns_uri::W && tag.local == "r"
                                })
                            })
                            .expect("body run");
                        let first = context.insert_inline_image(
                            target_run,
                            &media,
                            &InlineImageOptions::default(),
                        )?;
                        let second = context.insert_inline_image(
                            target_run,
                            &media,
                            &InlineImageOptions::default(),
                        )?;
                        let second_doc_pr = context
                            .story()
                            .document()
                            .descendants(second.drawing)
                            .into_iter()
                            .find(|node| {
                                context.story().document().tag(*node).is_some_and(|tag| {
                                    tag.ns == docxtpl_xml::ns_uri::WP && tag.local == "docPr"
                                })
                            })
                            .expect("second docPr");
                        context.story_mut().document_mut().set_attr(
                            second_doc_pr,
                            "",
                            "id",
                            first.doc_pr_id.to_string(),
                        );
                        Ok(())
                    },
                )?;
                Ok(())
            },
        )?;
        Ok(())
    })?;

    assert!(report.passes[0].rolled_back);
    assert!(report.passes[0].warnings[0]
        .message
        .contains("duplicate managed docPr id"));
    let reopened =
        Package::from_reader(Cursor::new(document.to_bytes()?), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(!xml.contains("<w:drawing"));
    assert!(reopened
        .relationships_of("word/document.xml")
        .is_none_or(|relationships| relationships
            .iter()
            .all(|relationship| !relationship.rel_type.ends_with("/image"))));
    Ok(())
}

#[test]
fn word_fragment_import_remaps_images_links_numbering_and_drawing_ids(
) -> Result<(), Box<dyn std::error::Error>> {
    const NUMBERING_REL: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/numbering";
    const NUMBERING_CT: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.numbering+xml";
    const NUMBERING_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:abstractNum w:abstractNumId="4"><w:nsid w:val="12345678"/><w:multiLevelType w:val="singleLevel"/><w:lvl w:ilvl="0"><w:start w:val="1"/><w:numFmt w:val="decimal"/><w:lvlText w:val="%1."/></w:lvl></w:abstractNum><w:num w:numId="7"><w:abstractNumId w:val="4"/></w:num></w:numbering>"#;

    let source_context: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/contexts/r3_docpr.json"
    )))?;
    let source_template = DocxTemplate::open(DRAWING_TEMPLATE)?;
    let mut source = source_template.render(&source_context, &RenderOptions::compat())?;
    source.postprocess(|pipeline| {
        pipeline.pass("source-link", FailurePolicy::Abort, |transaction| {
            let rid = transaction
                .relate_external_hyperlink("word/document.xml", "https://example.test/imported")?;
            transaction.for_each_story(StoryScope::Body, |story| {
                let drawing = story
                    .document()
                    .descendants(story.document().root())
                    .into_iter()
                    .find(|node| {
                        story.document().tag(*node).is_some_and(|tag| {
                            tag.ns == docxtpl_xml::ns_uri::W && tag.local == "drawing"
                        })
                    })
                    .expect("source drawing");
                story.attach_drawing_external_link(drawing, &rid)
            })?;
            Ok(())
        })?;
        Ok(())
    })?;
    source.edit_package(|package| {
        let mut transaction = package.transaction();
        if transaction.part("word/numbering.xml").is_some() {
            transaction.set_part_bytes("word/numbering.xml", NUMBERING_XML.as_bytes().to_vec())?;
        } else {
            transaction.add_part("word/numbering.xml", NUMBERING_XML.as_bytes().to_vec())?;
        }
        transaction.register_content_type("word/numbering.xml", NUMBERING_CT)?;
        transaction.get_or_add_relationship(
            "word/document.xml",
            NUMBERING_REL,
            "numbering.xml",
            TargetMode::Internal,
        )?;
        let xml = std::str::from_utf8(
            transaction
                .part("word/document.xml")
                .expect("source document")
                .bytes()?,
        )
        .expect("source document is UTF-8");
        let mut tree =
            docxtpl_xml::XmlDocument::parse_strict(xml, &docxtpl_xml::XmlLimits::default())
                .expect("source document parses");
        let paragraph = tree
            .descendants(tree.root())
            .into_iter()
            .find(|node| {
                tree.tag(*node)
                    .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p")
            })
            .expect("source paragraph");
        let p_pr = tree.new_w_element("pPr", vec![]).expect("pPr");
        let num_pr = tree.new_w_element("numPr", vec![]).expect("numPr");
        let ilvl = tree
            .new_w_element("ilvl", vec![("val".into(), "0".into())])
            .expect("ilvl");
        let num_id = tree
            .new_w_element("numId", vec![("val".into(), "7".into())])
            .expect("numId");
        tree.append_child(num_pr, ilvl);
        tree.append_child(num_pr, num_id);
        tree.append_child(p_pr, num_pr);
        tree.insert_child_at(paragraph, 0, p_pr);

        let bookmark_start = tree
            .new_w_element(
                "bookmarkStart",
                vec![
                    ("id".into(), "0".into()),
                    ("name".into(), "Imported Bookmark".into()),
                ],
            )
            .expect("bookmarkStart");
        let hyperlink = tree
            .new_w_element(
                "hyperlink",
                vec![("anchor".into(), "Imported Bookmark".into())],
            )
            .expect("hyperlink");
        let link_run = tree.new_w_element("r", vec![]).expect("link run");
        let link_text = tree.new_w_element("t", vec![]).expect("link text");
        tree.set_element_text(link_text, "fragment jump");
        tree.append_child(link_run, link_text);
        tree.append_child(hyperlink, link_run);
        let bookmark_end = tree
            .new_w_element("bookmarkEnd", vec![("id".into(), "0".into())])
            .expect("bookmarkEnd");
        tree.append_child(paragraph, bookmark_start);
        tree.append_child(paragraph, hyperlink);
        tree.append_child(paragraph, bookmark_end);

        let table = tree.new_w_element("tbl", vec![]).expect("table");
        let row = tree.new_w_element("tr", vec![]).expect("row");
        let cell = tree.new_w_element("tc", vec![]).expect("cell");
        let cell_paragraph = tree.new_w_element("p", vec![]).expect("cell paragraph");
        let cell_run = tree.new_w_element("r", vec![]).expect("cell run");
        let cell_text = tree.new_w_element("t", vec![]).expect("cell text");
        tree.set_element_text(cell_text, "fragment table cell");
        tree.append_child(cell_run, cell_text);
        tree.append_child(cell_paragraph, cell_run);
        tree.append_child(cell, cell_paragraph);
        tree.append_child(row, cell);
        tree.append_child(table, row);
        let body = tree
            .descendants(tree.root())
            .into_iter()
            .find(|node| {
                tree.tag(*node)
                    .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "body")
            })
            .expect("source body");
        let insert_position = tree
            .children(body)
            .iter()
            .position(|node| {
                tree.tag(*node)
                    .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "sectPr")
            })
            .unwrap_or(tree.children(body).len());
        tree.insert_child_at(body, insert_position, table);
        transaction.set_part_bytes("word/document.xml", tree.serialize().into_bytes())?;
        transaction.commit();
        Ok(())
    })?;
    let source_package =
        Package::from_reader(Cursor::new(source.to_bytes()?), &PackageLimits::default())?;
    let mut fragment = Some(WordFragment::from_package(
        source_package,
        "word/document.xml",
    )?);

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "target"}), &RenderOptions::compat())?;
    let mut import_report = None;
    document.postprocess(|pipeline| {
        pipeline.pass("import-wordml", FailurePolicy::Abort, |transaction| {
            transaction.for_each_editable_story_with_resources(
                EditableStorySelection::BODY,
                |context| {
                    let target = context
                        .story()
                        .document()
                        .descendants(context.story().document().root())
                        .into_iter()
                        .find(|node| {
                            context.story().document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p"
                            })
                        })
                        .expect("target paragraph");
                    import_report = Some(context.import_fragment(
                        target,
                        fragment.take().expect("body visited once"),
                        FragmentImportOptions::default(),
                    )?);
                    Ok(())
                },
            )?;
            Ok(())
        })?;
        Ok(())
    })?;

    let report = import_report.expect("fragment import report");
    assert!(report.inserted_nodes > 0);
    assert!(report.image_relationships > 0);
    assert_eq!(report.hyperlink_relationships, 1);
    assert_eq!(report.numbering_instances, 1);
    let reopened =
        Package::from_reader(Cursor::new(document.to_bytes()?), &PackageLimits::default())?;
    reopened.validate()?;
    let relationships = reopened
        .relationships_of("word/document.xml")
        .expect("target relationships");
    assert_eq!(
        relationships
            .iter()
            .filter(|relationship| relationship.target == "https://example.test/imported")
            .count(),
        1
    );
    assert!(relationships
        .iter()
        .any(|relationship| relationship.rel_type == NUMBERING_REL));
    let document_xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(document_xml.contains("<w:drawing"));
    assert!(document_xml.contains("fragment table cell"));
    assert!(document_xml.contains(r#"w:name="Imported_Bookmark""#));
    assert!(document_xml.contains(r#"w:anchor="Imported_Bookmark""#));
    assert!(!document_xml.contains(r#"w:numId w:val="7""#));
    let numbering = std::str::from_utf8(reopened.part("word/numbering.xml").unwrap().bytes()?)?;
    assert!(numbering.contains("<w:abstractNum"));
    assert!(numbering.contains("<w:num"));
    Ok(())
}

#[test]
fn unsupported_fragment_feature_rolls_back_before_target_mutation(
) -> Result<(), Box<dyn std::error::Error>> {
    let source_template = DocxTemplate::open(TEMPLATE)?;
    let mut source =
        source_template.render(&json!({"name": "chart source"}), &RenderOptions::compat())?;
    source.edit_package(|package| {
        let bytes = package.part("word/document.xml").unwrap().bytes()?;
        let xml = std::str::from_utf8(bytes).expect("source document is UTF-8");
        let changed = xml.replacen(
            "</w:r>",
            r#"<c:chart xmlns:c="http://schemas.openxmlformats.org/drawingml/2006/chart"/></w:r>"#,
            1,
        );
        package.set_part_bytes("word/document.xml", changed.into_bytes())?;
        Ok(())
    })?;
    let source_package =
        Package::from_reader(Cursor::new(source.to_bytes()?), &PackageLimits::default())?;
    let mut fragment = Some(WordFragment::from_package(
        source_package,
        "word/document.xml",
    )?);

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut target = template.render(&json!({"name": "unchanged"}), &RenderOptions::compat())?;
    let report = target.postprocess(|pipeline| {
        pipeline.pass(
            "reject-chart-fragment",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                transaction.for_each_editable_story_with_resources(
                    EditableStorySelection::BODY,
                    |context| {
                        let paragraph = context
                            .story()
                            .document()
                            .descendants(context.story().document().root())
                            .into_iter()
                            .find(|node| {
                                context.story().document().tag(*node).is_some_and(|tag| {
                                    tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p"
                                })
                            })
                            .expect("target paragraph");
                        context.import_fragment(
                            paragraph,
                            fragment.take().expect("body visited once"),
                            FragmentImportOptions::default(),
                        )?;
                        Ok(())
                    },
                )?;
                Ok(())
            },
        )?;
        Ok(())
    })?;
    assert!(report.passes[0].rolled_back);
    assert!(report.passes[0].warnings[0]
        .message
        .contains("unsupported WordML fragment feature \"chart\""));
    let reopened =
        Package::from_reader(Cursor::new(target.to_bytes()?), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(xml.contains("unchanged"));
    assert!(!xml.contains("chart source"));
    Ok(())
}

#[test]
fn word_fragment_import_supports_all_six_editable_story_kinds(
) -> Result<(), Box<dyn std::error::Error>> {
    const COMMENTS_CONTENT_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml";
    const COMMENTS_REL_TYPE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
    const COMMENTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="0" w:author="docxtpl-rs"><w:p><w:r><w:t>comment target</w:t></w:r></w:p></w:comment></w:comments>"#;

    let source_template = DocxTemplate::open(TEMPLATE)?;
    let source = source_template.render(
        &json!({"name": "six story imported text"}),
        &RenderOptions::compat(),
    )?;
    let source_bytes = source.to_bytes()?;
    let mut all_kinds = Vec::new();

    let context: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/contexts/p5_hf_basic.json"
    )))?;
    let template = DocxTemplate::open(STORY_TEMPLATE)?;
    let mut body_headers_footers = template.render(&context, &RenderOptions::compat())?;
    all_kinds.extend(import_simple_fragment_into_selection(
        &mut body_headers_footers,
        EditableStorySelection::BODY_HEADERS_FOOTERS,
        &source_bytes,
    )?);

    let template = DocxTemplate::open(NOTE_TEMPLATE)?;
    let mut notes = template.render(
        &json!({"a_jinja_variable": "A Jinja variable!"}),
        &RenderOptions::compat(),
    )?;
    all_kinds.extend(import_simple_fragment_into_selection(
        &mut notes,
        EditableStorySelection::NOTES,
        &source_bytes,
    )?);

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut comments = template.render(&json!({"name": "body"}), &RenderOptions::compat())?;
    comments.edit_package(|package| {
        let mut transaction = package.transaction();
        transaction.add_part("word/comments.xml", COMMENTS_XML.as_bytes().to_vec())?;
        transaction.register_content_type("word/comments.xml", COMMENTS_CONTENT_TYPE)?;
        transaction.get_or_add_relationship(
            "word/document.xml",
            COMMENTS_REL_TYPE,
            "comments.xml",
            TargetMode::Internal,
        )?;
        transaction.commit();
        Ok(())
    })?;
    all_kinds.extend(import_simple_fragment_into_selection(
        &mut comments,
        EditableStorySelection::COMMENTS,
        &source_bytes,
    )?);

    for kind in [
        EditableStoryKind::Body,
        EditableStoryKind::Header,
        EditableStoryKind::Footer,
        EditableStoryKind::Footnote,
        EditableStoryKind::Endnote,
        EditableStoryKind::Comment,
    ] {
        assert!(all_kinds.contains(&kind), "missing {kind:?}: {all_kinds:?}");
    }
    for document in [&body_headers_footers, &notes, &comments] {
        let bytes = document.to_bytes()?;
        let package = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
        package.validate()?;
        assert!(
            package
                .parts()
                .filter(|part| {
                    !part.is_dir()
                        && part.bytes().is_ok_and(|bytes| {
                            bytes
                                .windows(23)
                                .any(|window| window == b"six story imported text")
                        })
                })
                .count()
                > 0
        );
    }
    Ok(())
}

#[test]
fn story_resource_context_rolls_back_dom_media_and_relationship_together(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "original"}), &RenderOptions::compat())?;
    let media_bytes: Arc<[u8]> = Arc::from(std::fs::read(MEDIA)?);
    let mut attempted_part = String::new();

    let report = document.postprocess(|pipeline| {
        pipeline.pass(
            "story-resource-rollback",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                transaction.for_each_editable_story_with_resources(
                    EditableStorySelection::BODY,
                    |context| {
                        let media = context
                            .resources()
                            .register_media_bytes("rollback.png", Arc::clone(&media_bytes))?;
                        attempted_part = media.part_name.clone();
                        context.resources().relate_image(&media)?;
                        let text = context
                            .story()
                            .document()
                            .descendants(context.story().document().root())
                            .into_iter()
                            .find(|node| {
                                context.story().document().tag(*node).is_some_and(|tag| {
                                    tag.ns == docxtpl_xml::ns_uri::W && tag.local == "t"
                                })
                            })
                            .expect("body text");
                        context
                            .story_mut()
                            .document_mut()
                            .set_element_text(text, "rolled back story");
                        Ok(())
                    },
                )?;
                Err(docxtpl_rs::Error::Opc(docxtpl_opc::OpcError::Malformed {
                    reason: "force shared rollback".to_string(),
                }))
            },
        )?;
        Ok(())
    })?;

    assert!(report.passes[0].rolled_back);
    let reopened =
        Package::from_reader(Cursor::new(document.to_bytes()?), &PackageLimits::default())?;
    assert!(!reopened.contains(&attempted_part));
    let body = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(body.contains("original"));
    assert!(!body.contains("rolled back story"));
    assert!(reopened
        .relationships_of("word/document.xml")
        .is_none_or(|rels| !rels.iter().any(|rel| rel.rel_type.ends_with("/image"))));
    Ok(())
}

#[test]
fn unified_story_editor_supports_word_comments() -> Result<(), Box<dyn std::error::Error>> {
    const COMMENTS_CONTENT_TYPE: &str =
        "application/vnd.openxmlformats-officedocument.wordprocessingml.comments+xml";
    const COMMENTS_REL_TYPE: &str =
        "http://schemas.openxmlformats.org/officeDocument/2006/relationships/comments";
    const COMMENTS_XML: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:comments xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:comment w:id="0" w:author="docxtpl-rs"><w:p><w:r><w:t>comment target</w:t></w:r></w:p></w:comment></w:comments>"#;

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "body"}), &RenderOptions::compat())?;
    document.edit_package(|package| {
        let mut transaction = package.transaction();
        transaction.add_part("word/comments.xml", COMMENTS_XML.as_bytes().to_vec())?;
        transaction.register_content_type("word/comments.xml", COMMENTS_CONTENT_TYPE)?;
        transaction.get_or_add_relationship(
            "word/document.xml",
            COMMENTS_REL_TYPE,
            "comments.xml",
            TargetMode::Internal,
        )?;
        transaction.commit();
        Ok(())
    })?;
    let media_bytes: Arc<[u8]> = Arc::from(std::fs::read(MEDIA)?);

    document.postprocess(|pipeline| {
        pipeline.pass("edit-comment", FailurePolicy::Abort, |transaction| {
            let report = transaction.for_each_editable_story_with_resources(
                EditableStorySelection::COMMENTS,
                |context| {
                    let media = context
                        .resources()
                        .register_media_bytes("comment.png", Arc::clone(&media_bytes))?;
                    context.resources().relate_image(&media)?;
                    let story = context.story_mut();
                    assert_eq!(story.kind(), EditableStoryKind::Comment);
                    let paragraph = story
                        .document()
                        .descendants(story.document().root())
                        .into_iter()
                        .find(|node| {
                            story.document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p"
                            })
                        })
                        .expect("comment paragraph");
                    let index = story.run_text_index(paragraph, RunTextLimits::default())?;
                    let matched = index.find_literal("comment target")?.remove(0);
                    story.replace_text_match(
                        &index,
                        &matched,
                        "comment edited",
                        FormattingPolicy::RequireUniform,
                    )?;
                    Ok(())
                },
            )?;
            assert_eq!(report.parsed_parts, 1);
            assert_eq!(report.changed_parts, vec!["word/comments.xml"]);
            Ok(())
        })?;
        Ok(())
    })?;

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let comments = std::str::from_utf8(reopened.part("word/comments.xml").unwrap().bytes()?)?;
    assert!(comments.contains("comment edited"));
    assert!(!comments.contains("comment target"));
    Ok(())
}

#[test]
fn public_run_text_index_finds_and_replaces_literal_text() -> Result<(), Box<dyn std::error::Error>>
{
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "indexed"}), &RenderOptions::compat())?;

    document.postprocess(|pipeline| {
        pipeline.pass("replace-indexed", FailurePolicy::Abort, |transaction| {
            transaction.for_each_story(StoryScope::Body, |story| {
                let paragraphs: Vec<_> = story
                    .document()
                    .descendants(story.document().root())
                    .into_iter()
                    .filter(|node| {
                        story
                            .document()
                            .tag(*node)
                            .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p")
                    })
                    .collect();
                for paragraph in paragraphs {
                    let index = story.run_text_index(paragraph, RunTextLimits::default())?;
                    if let Some(matched) = index.find_literal("indexed")?.into_iter().next() {
                        story.replace_text_match(
                            &index,
                            &matched,
                            "replaced",
                            FormattingPolicy::InheritFirstRun,
                        )?;
                        return Ok(());
                    }
                }
                panic!("rendered body should contain indexed text")
            })?;
            Ok(())
        })?;
        Ok(())
    })?;

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(xml.contains("replaced"));
    assert!(!xml.contains("indexed"));
    Ok(())
}

#[test]
fn public_run_text_index_replaces_regex_with_format_overrides(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "item-42"}), &RenderOptions::compat())?;

    document.postprocess(|pipeline| {
        pipeline.pass("replace-regex", FailurePolicy::Abort, |transaction| {
            transaction.for_each_editable_story(EditableStorySelection::BODY, |story| {
                let paragraphs: Vec<_> = story
                    .document()
                    .descendants(story.document().root())
                    .into_iter()
                    .filter(|node| {
                        story
                            .document()
                            .tag(*node)
                            .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p")
                    })
                    .collect();
                let mut replacements = 0;
                for paragraph in paragraphs {
                    let index = story.run_text_index(paragraph, RunTextLimits::default())?;
                    replacements += story.replace_regex_all(
                        &index,
                        r"item-(\d+)",
                        "value-$1",
                        FormattingPolicy::InheritFirstRun,
                        &RunFormatOverrides::new().bold(true).color("336699"),
                    )?;
                }
                assert_eq!(replacements, 1);
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    })?;

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(xml.contains("value-42"));
    assert!(!xml.contains("item-42"));
    assert!(xml.contains("w:color w:val=\"336699\""));
    Ok(())
}

#[test]
fn cancellation_is_distinct_and_rolls_back_active_pass() -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        template
            .render_with_cancellation(
                &json!({"name": "value"}),
                &RenderOptions::compat(),
                &cancelled
            )
            .unwrap_err(),
        CancellationError::Cancelled
    ));

    let mut document = template.render(&json!({"name": "original"}), &RenderOptions::compat())?;
    let token = CancellationToken::new();
    let error = document
        .postprocess_with_cancellation(&token, |pipeline| {
            pipeline.pass("cancelled", FailurePolicy::WarnAndRollback, |transaction| {
                transaction.set_part_bytes("word/document.xml", b"broken".to_vec())?;
                token.cancel();
                Ok(())
            })?;
            Ok(())
        })
        .expect_err("cancellation must abort even with warning policy");
    assert!(matches!(error, CancellationError::Cancelled));

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(xml.contains("original"));
    Ok(())
}

#[test]
fn rendered_document_can_be_edited_before_its_only_write() -> Result<(), Box<dyn std::error::Error>>
{
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "before"}), &RenderOptions::compat())?;
    assert!(!document.is_edited());

    document.edit_package(|package| {
        let current = package
            .part("word/document.xml")
            .expect("document part")
            .bytes()?
            .to_vec();
        let xml = String::from_utf8(current)
            .expect("fixture XML is UTF-8")
            .replace("before", "after");
        package.set_part_bytes("word/document.xml", xml.into_bytes())?;
        Ok(())
    })?;
    assert!(document.is_edited());

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    reopened.validate()?;
    let xml = std::str::from_utf8(
        reopened
            .part("word/document.xml")
            .expect("document part")
            .bytes()?,
    )?;
    assert!(xml.contains("after"));
    assert!(!xml.contains("before"));
    Ok(())
}

#[test]
fn edited_document_is_revalidated_before_output() -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "value"}), &RenderOptions::compat())?;
    document.edit_package(|package| {
        let invalid_root_rels = concat!(
            r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">"#,
            r#"<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>"#,
            r#"<Relationship Id="rId2" Type="http://example.com/missing" Target="missing.xml"/>"#,
            "</Relationships>"
        );
        package.set_part_bytes("_rels/.rels", invalid_root_rels.as_bytes().to_vec())?;
        Ok(())
    })?;

    let error = document
        .to_bytes()
        .expect_err("dangling relationship must be caught before output");
    assert!(
        error.to_string().contains("does not match any part"),
        "got: {error}"
    );
    Ok(())
}

#[test]
fn a_failed_edit_still_requires_validation() -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "value"}), &RenderOptions::compat())?;
    let result: Result<(), docxtpl_rs::Error> = document.edit_package(|package| {
        package.set_part_bytes("word/document.xml", b"edited".to_vec())?;
        Err(docxtpl_rs::Error::Opc(docxtpl_opc::OpcError::Malformed {
            reason: "synthetic callback failure".to_string(),
        }))
    });
    assert!(result.is_err());
    assert!(document.is_edited());
    document.to_bytes()?;
    Ok(())
}

#[test]
fn postprocess_pipeline_commits_and_warns_with_pass_local_rollback(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "before"}), &RenderOptions::compat())?;

    let report = document.postprocess_with_metrics(|pipeline| {
        pipeline.pass("commit", FailurePolicy::Abort, |transaction| {
            let xml = String::from_utf8(
                transaction
                    .part("word/document.xml")
                    .expect("document part")
                    .bytes()?
                    .to_vec(),
            )
            .expect("fixture XML is UTF-8")
            .replace("before", "committed");
            transaction.set_part_bytes("word/document.xml", xml.into_bytes())?;
            Ok(())
        })?;
        pipeline.pass(
            "recoverable",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                transaction.set_part_bytes("word/document.xml", b"broken".to_vec())?;
                Err(docxtpl_rs::Error::Opc(docxtpl_opc::OpcError::Malformed {
                    reason: "recoverable test failure".to_string(),
                }))
            },
        )?;
        pipeline.pass("after-rollback", FailurePolicy::Abort, |transaction| {
            let xml = String::from_utf8(
                transaction
                    .part("word/document.xml")
                    .expect("document part")
                    .bytes()?
                    .to_vec(),
            )
            .expect("fixture XML is UTF-8");
            assert!(xml.contains("committed"));
            assert!(!xml.contains("broken"));
            Ok(())
        })?;
        Ok(())
    })?;

    assert_eq!(report.report.passes.len(), 3);
    assert!(report.report.passes[0].changed);
    assert_eq!(report.details[0].transaction.snapshotted_parts, 1);
    assert!(report.details[0].transaction.snapshotted_bytes > 0);
    assert!(report.report.passes[1].rolled_back);
    assert_eq!(report.details[1].transaction.snapshotted_parts, 1);
    assert!(report.details[1].rollback_elapsed > std::time::Duration::ZERO);
    assert_eq!(
        report.details[1].residency_after,
        report.details[2].residency_before
    );
    assert_eq!(report.report.passes[1].warnings[0].code, "pass_rolled_back");
    assert!(!report.report.passes[2].changed);
    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(
        reopened
            .part("word/document.xml")
            .expect("document part")
            .bytes()?,
    )?;
    assert!(xml.contains("committed"));
    Ok(())
}

#[test]
fn abort_policy_rolls_back_failing_pass_and_returns_error() -> Result<(), Box<dyn std::error::Error>>
{
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "original"}), &RenderOptions::compat())?;

    let error = document
        .postprocess(|pipeline| {
            pipeline.pass("abort", FailurePolicy::Abort, |transaction| {
                transaction.set_part_bytes("word/document.xml", b"broken".to_vec())?;
                Err(docxtpl_rs::Error::Opc(docxtpl_opc::OpcError::Malformed {
                    reason: "fatal test failure".to_string(),
                }))
            })?;
            Ok(())
        })
        .expect_err("abort policy should return the pass error");
    assert!(error.to_string().contains("fatal test failure"));

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(
        reopened
            .part("word/document.xml")
            .expect("document part")
            .bytes()?,
    )?;
    assert!(xml.contains("original"));
    assert!(!xml.contains("broken"));
    Ok(())
}

#[test]
fn story_editor_shares_one_dom_and_serializes_once() -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "initial"}), &RenderOptions::compat())?;
    let mut story_report = None;

    let report = document.postprocess(|pipeline| {
        pipeline.pass("two-dom-edits", FailurePolicy::Abort, |transaction| {
            story_report = Some(transaction.for_each_story_with_metrics(
                StoryScope::Body,
                |story| {
                    assert_eq!(story.kind(), StoryKind::Body);
                    let text_node = story
                        .document()
                        .descendants(story.document().root())
                        .into_iter()
                        .find(|id| {
                            story.document().tag(*id).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "t"
                            })
                        })
                        .expect("body contains a w:t element");
                    story.document_mut().set_element_text(text_node, "first");
                    story.document_mut().set_element_text(text_node, "second");
                    Ok(())
                },
            )?);
            Ok(())
        })?;
        Ok(())
    })?;

    let story_report = story_report.expect("story report");
    assert_eq!(story_report.edit.parsed_parts, 1);
    assert!(story_report.metrics.parsed_bytes > 0);
    assert!(story_report.metrics.parse_elapsed > std::time::Duration::ZERO);
    assert_eq!(story_report.edit.serialized_parts, 1);
    assert!(story_report.metrics.serialized_bytes > 0);
    assert!(story_report.metrics.serialize_elapsed > std::time::Duration::ZERO);
    assert_eq!(story_report.edit.changed_parts, vec!["word/document.xml"]);
    assert_eq!(report.passes[0].touched_parts, vec!["word/document.xml"]);

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(xml.contains("second"));
    assert!(!xml.contains("first"));
    Ok(())
}

#[test]
fn story_high_water_policy_evicts_reloadable_clean_parts() -> Result<(), Box<dyn std::error::Error>>
{
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "eviction"}), &RenderOptions::compat())?;

    let report = document.postprocess_with_metrics(|pipeline| {
        pipeline.set_part_cache_policy(PartCachePolicy::EvictAbove { resident_bytes: 0 });
        pipeline.pass("evict", FailurePolicy::Abort, |transaction| {
            transaction
                .part("[Content_Types].xml")
                .expect("content types")
                .bytes()?;
            transaction.for_each_story(StoryScope::Body, |_story| Ok(()))?;
            Ok(())
        })?;
        Ok(())
    })?;

    let pass = &report.details[0];
    assert!(pass.resources.eviction_runs >= 1);
    assert!(pass.resources.evicted_parts >= 1);
    assert!(pass.resources.evicted_bytes > 0);
    assert!(pass.resources.peak_resident_bytes >= pass.residency_before.resident_bytes);
    assert!(pass.resources.peak_resident_bytes >= pass.residency_after.resident_bytes);
    assert!(pass.residency_after.resident_bytes <= pass.residency_before.resident_bytes);
    document.to_bytes()?;
    Ok(())
}

#[test]
fn story_scope_visits_body_then_headers_then_footers_without_rewriting_reads(
) -> Result<(), Box<dyn std::error::Error>> {
    let context: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/contexts/p5_hf_basic.json"
    )))?;
    let template = DocxTemplate::open(STORY_TEMPLATE)?;
    let mut document = template.render(&context, &RenderOptions::compat())?;
    let mut kinds = Vec::new();
    let mut story_report = None;

    document.postprocess(|pipeline| {
        pipeline.pass("inspect-stories", FailurePolicy::Abort, |transaction| {
            story_report = Some(transaction.for_each_story(
                StoryScope::BodyHeadersFooters,
                |story| {
                    kinds.push(story.kind());
                    Ok(())
                },
            )?);
            Ok(())
        })?;
        Ok(())
    })?;

    assert_eq!(kinds.first(), Some(&StoryKind::Body));
    let first_footer = kinds
        .iter()
        .position(|kind| *kind == StoryKind::Footer)
        .expect("fixture has a footer");
    assert!(kinds[1..first_footer]
        .iter()
        .all(|kind| *kind == StoryKind::Header));
    assert!(kinds[first_footer..]
        .iter()
        .all(|kind| *kind == StoryKind::Footer));
    let story_report = story_report.expect("story report");
    assert_eq!(story_report.parsed_parts, kinds.len());
    assert_eq!(story_report.serialized_parts, 0);
    assert!(story_report.changed_parts.is_empty());
    Ok(())
}

#[test]
fn render_report_combines_pass_and_single_zip_write_metrics(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "metric"}), &RenderOptions::compat())?;
    document.postprocess(|pipeline| {
        pipeline.pass("inspect", FailurePolicy::Abort, |transaction| {
            transaction.for_each_story(StoryScope::Body, |_story| Ok(()))?;
            Ok(())
        })?;
        Ok(())
    })?;
    let residency_before = document.residency();
    let eviction = document.evict_clean_part_caches();
    assert_eq!(eviction.before, residency_before);
    assert!(eviction.after.resident_bytes <= residency_before.resident_bytes);
    let mut output = Cursor::new(Vec::new());

    let report =
        document.write_to_with_report(&mut output, &docxtpl_rs::WriteOptions::compatible())?;

    assert_eq!(report.postprocess_passes.len(), 1);
    assert_eq!(report.postprocess_passes[0].name, "inspect");
    assert_eq!(
        report.package_write.output_bytes,
        output.get_ref().len() as u64
    );
    assert_eq!(
        report.package_write.raw_copied_parts + report.package_write.rewritten_parts,
        report.package_write.total_parts
    );
    assert!(report.package_write.total_parts > 0);
    assert_eq!(
        report.package_write.resident_bytes,
        eviction.after.resident_bytes
    );
    assert!(report.render_elapsed > std::time::Duration::ZERO);
    Ok(())
}

#[test]
fn media_registration_deduplicates_and_relates_multiple_owners(
) -> Result<(), Box<dyn std::error::Error>> {
    let context: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/contexts/p5_hf_basic.json"
    )))?;
    let template = DocxTemplate::open(STORY_TEMPLATE)?;
    let mut document = template.render(&context, &RenderOptions::compat())?;
    let mut registered_part = String::new();
    let mut body_rid = String::new();

    let report = document.postprocess_with_metrics(|pipeline| {
        pipeline.pass("media", FailurePolicy::Abort, |transaction| {
            let first = transaction.register_media_path(MEDIA)?;
            assert!(!first.reused);
            let second = transaction.register_media_path(MEDIA)?;
            assert!(second.reused);
            assert_eq!(first.part_name, second.part_name);
            registered_part = first.part_name.clone();

            body_rid = transaction.relate_image("word/document.xml", &first)?;
            assert_eq!(
                body_rid,
                transaction.relate_image("word/document.xml", &second)?
            );
            transaction.relate_image("word/header1.xml", &first)?;
            transaction.validate()?;
            Ok(())
        })?;
        Ok(())
    })?;

    assert_eq!(report.details[0].resources.media_added, 1);
    assert_eq!(report.details[0].resources.media_reused, 1);
    assert_eq!(report.details[0].resources.relationships_added, 2);
    assert_eq!(report.details[0].resources.relationships_reused, 1);

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    reopened.validate()?;
    assert!(reopened.contains(&registered_part));
    assert!(reopened
        .relationships_of("word/document.xml")
        .and_then(|rels| rels.get(&body_rid))
        .is_some());
    assert!(reopened
        .relationships_of("word/header1.xml")
        .is_some_and(|rels| rels.iter().any(|rel| rel.rel_type.ends_with("/image"))));
    Ok(())
}

#[test]
fn byte_file_and_path_media_sources_share_dedup_and_detect_content(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document =
        template.render(&json!({"name": "media sources"}), &RenderOptions::compat())?;
    let bytes: Arc<[u8]> = Arc::from(std::fs::read(MEDIA)?);
    let original_ptr = bytes.as_ptr();
    let mut registered_part = String::new();

    document.postprocess(|pipeline| {
        pipeline.pass("media-sources", FailurePolicy::Abort, |transaction| {
            let from_bytes =
                transaction.register_media_bytes("misleading.jpeg", Arc::clone(&bytes))?;
            assert!(!from_bytes.reused);
            assert!(from_bytes.part_name.ends_with(".png"));
            assert_eq!(
                transaction
                    .part(&from_bytes.part_name)
                    .unwrap()
                    .bytes()?
                    .as_ptr(),
                original_ptr,
                "shared bytes must be retained without copying"
            );

            let from_path = transaction.register_media_path(MEDIA)?;
            assert!(from_path.reused);
            assert_eq!(from_path.part_name, from_bytes.part_name);

            let snapshot = FilePartSource::snapshot(MEDIA)?;
            let from_source =
                transaction.register_media_source("also-wrong.gif", MediaSource::File(snapshot))?;
            assert!(from_source.reused);
            assert_eq!(from_source.part_name, from_bytes.part_name);
            registered_part = from_bytes.part_name;
            Ok(())
        })?;
        Ok(())
    })?;

    let reopened =
        Package::from_reader(Cursor::new(document.to_bytes()?), &PackageLimits::default())?;
    reopened.validate()?;
    assert!(reopened.contains(&registered_part));
    Ok(())
}

#[test]
fn media_catalog_is_shared_across_passes_and_restored_after_rollback(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "catalog"}), &RenderOptions::compat())?;
    let first: Arc<[u8]> = Arc::from(std::fs::read(MEDIA)?);
    let second: Arc<[u8]> = Arc::from(std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/media/p4_wide4x1.png"
    ))?);
    let mut rolled_back_part = String::new();
    let mut retried_part = String::new();
    let mut metrics = None;

    document.postprocess(|pipeline| {
        pipeline.pass("catalog-seed", FailurePolicy::Abort, |transaction| {
            let media = transaction.register_media_bytes("first.png", Arc::clone(&first))?;
            assert!(!media.reused);
            Ok(())
        })?;
        pipeline.pass(
            "catalog-rollback",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                let media = transaction.register_media_bytes("second.png", Arc::clone(&second))?;
                rolled_back_part = media.part_name;
                Err(docxtpl_rs::Error::Opc(docxtpl_opc::OpcError::Malformed {
                    reason: "force catalog rollback".to_string(),
                }))
            },
        )?;
        pipeline.pass("catalog-retry", FailurePolicy::Abort, |transaction| {
            let media = transaction.register_media_bytes("second.png", Arc::clone(&second))?;
            assert!(!media.reused, "rolled-back media must not remain cached");
            retried_part = media.part_name;
            let reused = transaction.register_media_bytes("first.png", Arc::clone(&first))?;
            assert!(reused.reused);
            Ok(())
        })?;
        metrics = Some(pipeline.media_catalog_metrics());
        Ok(())
    })?;

    assert_eq!(rolled_back_part, retried_part);
    let metrics = metrics.expect("catalog metrics");
    assert_eq!(metrics.scanned_parts, 0);
    assert_eq!(metrics.hashed_bytes, 0);
    assert!(metrics.cache_hits >= 3, "metrics: {metrics:?}");
    assert!(metrics.reused_media >= 1, "metrics: {metrics:?}");
    document.to_bytes()?;
    Ok(())
}

#[test]
fn probed_media_file_reuses_probe_and_detects_late_source_changes(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("photo.data");
    std::fs::copy(MEDIA, &path)?;
    let probed = ProbedMediaFile::open(&path, &ResourceLimits::default())?;
    assert_eq!(probed.info().ext, "png");
    assert_eq!(probed.len(), std::fs::metadata(&path)?.len());

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "probed"}), &RenderOptions::compat())?;
    let mut metrics = None;
    document.postprocess(|pipeline| {
        pipeline.pass("probed-one", FailurePolicy::Abort, |transaction| {
            assert!(!transaction.register_probed_media(&probed)?.reused);
            Ok(())
        })?;
        pipeline.pass("probed-two", FailurePolicy::Abort, |transaction| {
            assert!(transaction.register_probed_media(&probed)?.reused);
            Ok(())
        })?;
        metrics = Some(pipeline.media_catalog_metrics());
        Ok(())
    })?;
    let metrics = metrics.expect("catalog metrics");
    assert_eq!(metrics.scanned_parts, 0);
    assert_eq!(metrics.hashed_bytes, 0);
    assert!(metrics.cache_hits >= 1);
    document.to_bytes()?;

    std::fs::write(&path, b"changed after registration")?;
    assert!(document.to_bytes().is_err());
    Ok(())
}

#[test]
fn structured_postprocess_error_preserves_code_and_rolls_back_shared_media(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "structured"}), &RenderOptions::compat())?;
    let bytes: Arc<[u8]> = Arc::from(std::fs::read(MEDIA)?);
    let mut attempted_part = String::new();

    let report = document.postprocess_structured(|pipeline| {
        pipeline.pass_structured(
            "structured-rollback",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                let media = transaction.register_media_bytes("photo.bin", Arc::clone(&bytes))?;
                attempted_part = media.part_name;
                Err(PostprocessError::custom(
                    "fragment.image_placeholder_missing",
                    "fragment image has no registered source",
                )
                .with_part("word/document.xml")
                .into())
            },
        )?;
        Ok(())
    })?;

    let pass = &report.passes[0];
    assert!(pass.rolled_back);
    assert_eq!(pass.warnings[0].code, "fragment.image_placeholder_missing");
    assert_eq!(
        pass.warnings[0].message,
        "fragment image has no registered source"
    );
    assert!(pass.touched_parts.contains(&attempted_part));

    let reopened =
        Package::from_reader(Cursor::new(document.to_bytes()?), &PackageLimits::default())?;
    assert!(!reopened.contains(&attempted_part));
    Ok(())
}

#[test]
fn structured_abort_returns_typed_custom_error() -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "abort"}), &RenderOptions::compat())?;

    let error = document
        .postprocess_structured(|pipeline| {
            pipeline.pass_structured("validate", FailurePolicy::Abort, |_transaction| {
                Err(
                    PostprocessError::custom("input.missing", "required input is missing")
                        .with_part("word/document.xml")
                        .into(),
                )
            })?;
            Ok(())
        })
        .expect_err("custom validation must abort");

    let PostprocessRunError::Custom(error) = error else {
        panic!("expected typed custom error")
    };
    assert_eq!(error.code(), "input.missing");
    assert_eq!(error.message(), "required input is missing");
    assert_eq!(error.part(), Some("word/document.xml"));
    Ok(())
}

#[test]
fn changed_file_media_snapshot_is_rejected_before_mutation(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("upload.png");
    std::fs::copy(MEDIA, &path)?;
    let snapshot = FilePartSource::snapshot(&path)?;
    std::fs::write(
        &path,
        std::fs::read(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/fixtures/media/p4_wide4x1.png"
        ))?,
    )?;

    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "changed"}), &RenderOptions::compat())?;
    let report = document.postprocess(|pipeline| {
        pipeline.pass(
            "changed-source",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                transaction.register_media_source("upload.png", MediaSource::File(snapshot))?;
                Ok(())
            },
        )?;
        Ok(())
    })?;

    assert!(report.passes[0].rolled_back);
    assert!(report.passes[0].touched_parts.is_empty());
    Ok(())
}

#[test]
fn invalid_byte_media_is_rejected_without_package_changes() -> Result<(), Box<dyn std::error::Error>>
{
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document =
        template.render(&json!({"name": "invalid media"}), &RenderOptions::compat())?;

    let report = document.postprocess(|pipeline| {
        pipeline.pass(
            "invalid-media",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                transaction
                    .register_media_bytes("looks-like.png", Arc::from(&b"not an image"[..]))?;
                Ok(())
            },
        )?;
        Ok(())
    })?;

    assert!(report.passes[0].rolled_back);
    assert!(!report.passes[0].changed);
    assert!(report.passes[0].touched_parts.is_empty());
    assert_eq!(report.passes[0].warnings[0].code, "pass_rolled_back");
    Ok(())
}

#[test]
fn failed_media_registration_rolls_back_part_relationship_and_content_type(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "rollback"}), &RenderOptions::compat())?;
    let mut attempted_part = String::new();

    let report = document.postprocess(|pipeline| {
        pipeline.pass(
            "media-rollback",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                let media = transaction.register_media_path(MEDIA)?;
                attempted_part = media.part_name.clone();
                transaction.relate_image("word/document.xml", &media)?;
                Err(docxtpl_rs::Error::Opc(docxtpl_opc::OpcError::Malformed {
                    reason: "force media rollback".to_string(),
                }))
            },
        )?;
        Ok(())
    })?;

    assert!(report.passes[0].rolled_back, "report: {report:?}");
    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    reopened.validate()?;
    assert!(!reopened.contains(&attempted_part));
    assert!(reopened
        .relationships_of("word/document.xml")
        .is_none_or(|rels| !rels.iter().any(|rel| rel.rel_type.ends_with("/image"))));
    Ok(())
}

#[test]
fn bookmark_and_internal_link_creation_is_deterministic_and_idempotent(
) -> Result<(), Box<dyn std::error::Error>> {
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "linked"}), &RenderOptions::compat())?;
    let mut created = None;

    document.postprocess(|pipeline| {
        pipeline.pass("internal-link", FailurePolicy::Abort, |transaction| {
            transaction.for_each_story(StoryScope::Body, |story| {
                let run = story
                    .document()
                    .descendants(story.document().root())
                    .into_iter()
                    .find(|node| {
                        story
                            .document()
                            .tag(*node)
                            .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "r")
                    })
                    .expect("body has a run");
                let bookmark = story.get_or_create_bookmark(run, "1 defect target")?;
                assert_eq!(bookmark.name, "_1_defect_target");
                assert_eq!(
                    bookmark,
                    story.get_or_create_bookmark(run, "1 defect target")?
                );
                let hyperlink = story.attach_internal_link(run, &bookmark)?;
                assert_eq!(hyperlink, story.attach_internal_link(run, &bookmark)?);
                created = Some(bookmark);
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    })?;

    let bookmark = created.expect("bookmark created");
    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert_eq!(xml.matches("<w:bookmarkStart").count(), 1);
    assert_eq!(xml.matches("<w:bookmarkEnd").count(), 1);
    assert_eq!(xml.matches("<w:hyperlink").count(), 1);
    assert!(xml.contains(&format!(r#"w:anchor="{}""#, bookmark.name)));
    Ok(())
}

#[test]
fn dangling_internal_link_fails_the_pass_and_rolls_back() -> Result<(), Box<dyn std::error::Error>>
{
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut document = template.render(&json!({"name": "linked"}), &RenderOptions::compat())?;

    let report = document.postprocess(|pipeline| {
        pipeline.pass(
            "dangling-link",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                transaction.for_each_story(StoryScope::Body, |story| {
                    let run = story
                        .document()
                        .descendants(story.document().root())
                        .into_iter()
                        .find(|node| {
                            story.document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "r"
                            })
                        })
                        .expect("body has a run");
                    story.attach_internal_link(
                        run,
                        &Bookmark {
                            id: "999".to_string(),
                            name: "missing_target".to_string(),
                        },
                    )?;
                    assert!(story
                        .document()
                        .descendants(story.document().root())
                        .into_iter()
                        .any(|node| {
                            story
                                .document()
                                .attr(node, docxtpl_xml::ns_uri::W, "anchor")
                                == Some("missing_target")
                        }));
                    Ok(())
                })?;
                Ok(())
            },
        )?;
        Ok(())
    })?;

    assert!(report.passes[0].rolled_back, "report: {report:?}");
    assert!(report.passes[0].warnings[0]
        .message
        .contains("missing bookmark"));
    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(!xml.contains("missing_target"));
    Ok(())
}

#[test]
fn existing_drawing_external_link_registers_once_and_updates_both_properties(
) -> Result<(), Box<dyn std::error::Error>> {
    let context: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/contexts/r3_docpr.json"
    )))?;
    let template = DocxTemplate::open(DRAWING_TEMPLATE)?;
    let mut document = template.render(&context, &RenderOptions::compat())?;
    let mut relationship_id = String::new();

    document.postprocess(|pipeline| {
        pipeline.pass("drawing-link", FailurePolicy::Abort, |transaction| {
            relationship_id = transaction
                .relate_external_hyperlink("word/document.xml", "https://example.test/video")?;
            assert_eq!(
                relationship_id,
                transaction
                    .relate_external_hyperlink("word/document.xml", "https://example.test/video")?
            );
            transaction.for_each_story(StoryScope::Body, |story| {
                let drawing = story
                    .document()
                    .descendants(story.document().root())
                    .into_iter()
                    .find(|node| {
                        story.document().tag(*node).is_some_and(|tag| {
                            tag.ns == docxtpl_xml::ns_uri::W && tag.local == "drawing"
                        })
                    })
                    .expect("fixture has a drawing");
                story.attach_drawing_external_link(drawing, &relationship_id)?;
                story.attach_drawing_external_link(drawing, &relationship_id)?;
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    })?;

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    reopened.validate()?;
    let relationships = reopened
        .relationships_of("word/document.xml")
        .expect("document relationships");
    assert_eq!(
        relationships
            .iter()
            .filter(|relationship| relationship.target == "https://example.test/video")
            .count(),
        1
    );
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert_eq!(xml.matches("<a:hlinkClick").count(), 2);
    assert_eq!(
        xml.matches(&format!(r#"r:id="{relationship_id}""#)).count(),
        2
    );
    Ok(())
}

fn exercise_fragment_remap_property(
    source_id: u16,
    url_suffix: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let source_context: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/contexts/r3_docpr.json"
    )))?;
    let source_template = DocxTemplate::open(DRAWING_TEMPLATE)?;
    let mut source = source_template.render(&source_context, &RenderOptions::compat())?;
    let url = format!("https://example.test/property/{url_suffix}");
    let bookmark_name = format!("property_{source_id}");
    source.postprocess(|pipeline| {
        pipeline.pass("property-source", FailurePolicy::Abort, |transaction| {
            let rid = transaction.relate_external_hyperlink("word/document.xml", &url)?;
            transaction.for_each_story(StoryScope::Body, |story| {
                let drawing = story
                    .document()
                    .descendants(story.document().root())
                    .into_iter()
                    .find(|node| {
                        story.document().tag(*node).is_some_and(|tag| {
                            tag.ns == docxtpl_xml::ns_uri::W && tag.local == "drawing"
                        })
                    })
                    .expect("property source drawing");
                story.attach_drawing_external_link(drawing, &rid)?;
                let run = story
                    .document()
                    .descendants(story.document().root())
                    .into_iter()
                    .find(|node| {
                        story
                            .document()
                            .tag(*node)
                            .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "r")
                    })
                    .expect("property source run");
                story.get_or_create_bookmark(run, &bookmark_name)?;

                let nodes = story.document().descendants(story.document().root());
                for node in nodes {
                    let Some(tag) = story.document().tag(node) else {
                        continue;
                    };
                    let replacement = if (tag.ns == docxtpl_xml::ns_uri::WP && tag.local == "docPr")
                        || (tag.ns == docxtpl_xml::ns_uri::PIC && tag.local == "cNvPr")
                    {
                        Some(u64::from(source_id) + 1)
                    } else if tag.ns == docxtpl_xml::ns_uri::W
                        && matches!(tag.local.as_str(), "bookmarkStart" | "bookmarkEnd")
                    {
                        Some(u64::from(source_id))
                    } else {
                        None
                    };
                    if let Some(replacement) = replacement {
                        let namespace = if story.document().tag(node).is_some_and(|tag| {
                            tag.ns == docxtpl_xml::ns_uri::W
                                && matches!(tag.local.as_str(), "bookmarkStart" | "bookmarkEnd")
                        }) {
                            docxtpl_xml::ns_uri::W
                        } else {
                            ""
                        };
                        story.document_mut().set_attr(
                            node,
                            namespace,
                            "id",
                            replacement.to_string(),
                        );
                    }
                }
                Ok(())
            })?;
            Ok(())
        })?;
        Ok(())
    })?;

    let source_package =
        Package::from_reader(Cursor::new(source.to_bytes()?), &PackageLimits::default())?;
    let mut fragment = Some(WordFragment::from_package(
        source_package,
        "word/document.xml",
    )?);
    let template = DocxTemplate::open(TEMPLATE)?;
    let mut target = template.render(
        &json!({"name": "property target"}),
        &RenderOptions::compat(),
    )?;
    target.postprocess(|pipeline| {
        pipeline.pass("property-import", FailurePolicy::Abort, |transaction| {
            transaction.for_each_editable_story_with_resources(
                EditableStorySelection::BODY,
                |context| {
                    let paragraph = context
                        .story()
                        .document()
                        .descendants(context.story().document().root())
                        .into_iter()
                        .find(|node| {
                            context.story().document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "p"
                            })
                        })
                        .expect("property target paragraph");
                    context.import_fragment(
                        paragraph,
                        fragment.take().expect("body visited once"),
                        FragmentImportOptions::default(),
                    )?;
                    Ok(())
                },
            )?;
            Ok(())
        })?;
        Ok(())
    })?;

    let reopened =
        Package::from_reader(Cursor::new(target.to_bytes()?), &PackageLimits::default())?;
    reopened.validate()?;
    let relationships = reopened
        .relationships_of("word/document.xml")
        .expect("imported relationships");
    assert_eq!(
        relationships
            .iter()
            .filter(|relationship| relationship.target == url)
            .count(),
        1
    );
    let image = relationships
        .iter()
        .find(|relationship| relationship.rel_type.ends_with("/image"))
        .expect("imported image relationship");
    let owner = docxtpl_opc::PartUri::new("word/document.xml")?;
    let image_part = docxtpl_opc::resolve_part_target(owner.parent().as_ref(), &image.target)
        .expect("image target resolves inside package");
    assert!(reopened.contains(image_part.as_str()));
    assert_eq!(
        reopened.content_types().content_type_of(&image_part),
        Some("image/png")
    );

    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    let tree = docxtpl_xml::XmlDocument::parse_strict(xml, &docxtpl_xml::XmlLimits::default())?;
    for (namespace, local) in [
        (docxtpl_xml::ns_uri::WP, "docPr"),
        (docxtpl_xml::ns_uri::PIC, "cNvPr"),
    ] {
        let ids: Vec<_> = tree
            .descendants(tree.root())
            .into_iter()
            .filter(|node| {
                tree.tag(*node)
                    .is_some_and(|tag| tag.ns == namespace && tag.local == local)
            })
            .map(|node| tree.attr(node, "", "id").unwrap().to_string())
            .collect();
        assert_eq!(
            ids.len(),
            ids.iter().collect::<std::collections::HashSet<_>>().len()
        );
    }
    let starts: std::collections::HashSet<_> = tree
        .descendants(tree.root())
        .into_iter()
        .filter(|node| {
            tree.tag(*node)
                .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "bookmarkStart")
        })
        .filter_map(|node| tree.attr(node, docxtpl_xml::ns_uri::W, "id"))
        .collect();
    let ends: std::collections::HashSet<_> = tree
        .descendants(tree.root())
        .into_iter()
        .filter(|node| {
            tree.tag(*node)
                .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "bookmarkEnd")
        })
        .filter_map(|node| tree.attr(node, docxtpl_xml::ns_uri::W, "id"))
        .collect();
    assert_eq!(starts, ends);
    Ok(())
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config {
        cases: 12,
        failure_persistence: None,
        ..proptest::test_runner::Config::default()
    })]

    #[test]
    fn fragment_relationship_content_type_and_ids_remain_valid(
        source_id in 0u16..4096,
        url_suffix in "[a-z0-9]{1,12}",
    ) {
        exercise_fragment_remap_property(source_id, &url_suffix).unwrap();
    }
}

#[test]
fn invalid_drawing_relationship_rolls_back_registered_external_link(
) -> Result<(), Box<dyn std::error::Error>> {
    let context: serde_json::Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/contexts/r3_docpr.json"
    )))?;
    let template = DocxTemplate::open(DRAWING_TEMPLATE)?;
    let mut document = template.render(&context, &RenderOptions::compat())?;

    let report = document.postprocess(|pipeline| {
        pipeline.pass(
            "invalid-drawing-link",
            FailurePolicy::WarnAndRollback,
            |transaction| {
                transaction.relate_external_hyperlink(
                    "word/document.xml",
                    "https://example.test/rolled-back",
                )?;
                transaction.for_each_story(StoryScope::Body, |story| {
                    let drawing = story
                        .document()
                        .descendants(story.document().root())
                        .into_iter()
                        .find(|node| {
                            story.document().tag(*node).is_some_and(|tag| {
                                tag.ns == docxtpl_xml::ns_uri::W && tag.local == "drawing"
                            })
                        })
                        .expect("fixture has a drawing");
                    story.attach_drawing_external_link(drawing, "rIdMissing")
                })?;
                Ok(())
            },
        )?;
        Ok(())
    })?;

    assert!(report.passes[0].rolled_back);
    assert!(report.passes[0].warnings[0]
        .message
        .contains("no external hyperlink relationship"));
    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    assert!(reopened
        .relationships_of("word/document.xml")
        .is_none_or(|relationships| relationships
            .iter()
            .all(|relationship| relationship.target != "https://example.test/rolled-back")));
    Ok(())
}

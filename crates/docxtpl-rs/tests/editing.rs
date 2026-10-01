use std::io::Cursor;

use docxtpl_rs::{
    Bookmark, CancellationError, CancellationToken, DocxTemplate, FailurePolicy, FormattingPolicy,
    Package, PackageLimits, RenderOptions, RunTextLimits, StoryKind, StoryScope,
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

    let report = document.postprocess(|pipeline| {
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

    assert_eq!(report.passes.len(), 3);
    assert!(report.passes[0].changed);
    assert!(report.passes[1].rolled_back);
    assert_eq!(report.passes[1].warnings[0].code, "pass_rolled_back");
    assert!(!report.passes[2].changed);
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
            story_report = Some(transaction.for_each_story(StoryScope::Body, |story| {
                assert_eq!(story.kind(), StoryKind::Body);
                let text_node = story
                    .document()
                    .descendants(story.document().root())
                    .into_iter()
                    .find(|id| {
                        story
                            .document()
                            .tag(*id)
                            .is_some_and(|tag| tag.ns == docxtpl_xml::ns_uri::W && tag.local == "t")
                    })
                    .expect("body contains a w:t element");
                story.document_mut().set_element_text(text_node, "first");
                story.document_mut().set_element_text(text_node, "second");
                Ok(())
            })?);
            Ok(())
        })?;
        Ok(())
    })?;

    let story_report = story_report.expect("story report");
    assert_eq!(story_report.parsed_parts, 1);
    assert_eq!(story_report.serialized_parts, 1);
    assert_eq!(story_report.changed_parts, vec!["word/document.xml"]);
    assert_eq!(report.passes[0].touched_parts, vec!["word/document.xml"]);

    let bytes = document.to_bytes()?;
    let reopened = Package::from_reader(Cursor::new(bytes), &PackageLimits::default())?;
    let xml = std::str::from_utf8(reopened.part("word/document.xml").unwrap().bytes()?)?;
    assert!(xml.contains("second"));
    assert!(!xml.contains("first"));
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

    document.postprocess(|pipeline| {
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

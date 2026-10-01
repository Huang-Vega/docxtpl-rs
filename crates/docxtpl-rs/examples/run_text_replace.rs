use docxtpl_rs::{
    DocxTemplate, FailurePolicy, FormattingPolicy, RenderOptions, RunTextLimits, StoryScope,
};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let template_path = args
        .next()
        .ok_or("usage: run_text_replace <template.docx> <output.docx>")?;
    let output_path = args
        .next()
        .ok_or("usage: run_text_replace <template.docx> <output.docx>")?;
    if args.next().is_some() {
        return Err("usage: run_text_replace <template.docx> <output.docx>".into());
    }

    let template = DocxTemplate::open(template_path)?;
    let mut document =
        template.render(&json!({"name": "visible target"}), &RenderOptions::compat())?;

    document.postprocess(|pipeline| {
        pipeline.pass(
            "replace-visible-text",
            FailurePolicy::Abort,
            |transaction| {
                transaction.for_each_story(StoryScope::BodyHeadersFooters, |story| {
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
                        let matches = index.find_literal("visible target")?;
                        if let Some(matched) = matches.first() {
                            story.replace_text_match(
                                &index,
                                matched,
                                "replacement accepted",
                                FormattingPolicy::InheritFirstRun,
                            )?;
                        }
                    }
                    Ok(())
                })?;
                Ok(())
            },
        )?;
        Ok(())
    })?;

    document.save(output_path)?;
    Ok(())
}

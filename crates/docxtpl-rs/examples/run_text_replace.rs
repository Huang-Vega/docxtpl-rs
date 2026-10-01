use docxtpl_rs::{
    DocxTemplate, EditableStorySelection, FailurePolicy, FormattingPolicy, RenderOptions,
    RunFormatOverrides, RunTextLimits,
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
                transaction.for_each_editable_story(EditableStorySelection::ALL, |story| {
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
                        story.replace_regex_all(
                            &index,
                            r"visible\s+target",
                            "replacement accepted",
                            FormattingPolicy::InheritFirstRun,
                            &RunFormatOverrides::new().bold(true).color("336699"),
                        )?;
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

//! Lists undeclared top-level variables from a DOCX template.

use docxtpl_rs::DocxTemplate;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args()
        .nth(1)
        .ok_or("usage: template_variables TEMPLATE")?;
    let template = DocxTemplate::open(path)?;
    for variable in template.undeclared_variables()? {
        println!("{variable}");
    }
    Ok(())
}

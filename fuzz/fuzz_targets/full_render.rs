#![no_main]

use docxtpl_rs::{DocxTemplate, RenderOptions};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(template) = DocxTemplate::from_bytes(data.to_vec()) {
        let _ = template.render(&serde_json::json!({}), &RenderOptions::compat());
    }
});

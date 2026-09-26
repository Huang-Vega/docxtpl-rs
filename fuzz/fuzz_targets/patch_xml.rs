#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &str| { let patched = docxtpl_compat::patch_xml(data); let _ = docxtpl_compat::resolve_listing(&patched); });

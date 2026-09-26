#![no_main]
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &str| { let limits = docxtpl_xml::XmlLimits { max_depth: 64 }; let _ = docxtpl_xml::XmlDocument::parse_strict(data, &limits); let _ = docxtpl_xml::XmlDocument::parse_lenient(data, &limits); });

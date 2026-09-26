#![no_main]
use std::io::Cursor;
use libfuzzer_sys::fuzz_target;
fuzz_target!(|data: &[u8]| { let _ = docxtpl_opc::Package::from_reader(Cursor::new(data), &docxtpl_opc::PackageLimits::default()); });

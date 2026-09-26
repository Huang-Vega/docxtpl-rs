//! P7c property tests: random/truncated ZIP bytes entering the OPC reader
//! must not panic.

mod common;

use std::io::Cursor;

use docxtpl_opc::{Package, PackageLimits};
use proptest::prelude::*;

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    #[test]
    fn arbitrary_package_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..8192)) {
        let _ = Package::from_reader(Cursor::new(bytes), &PackageLimits::default());
    }

    #[test]
    fn every_truncation_of_a_valid_package_is_handled(cut in 0usize..4096) {
        let bytes = common::minimal_docx();
        let end = cut.min(bytes.len());
        let _ = Package::from_reader(Cursor::new(&bytes[..end]), &PackageLimits::default());
    }
}

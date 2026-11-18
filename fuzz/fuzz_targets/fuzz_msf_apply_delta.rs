#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::msf::{MsfCatalog, MsfCatalogDocument, parse_catalog_namespace};

// 任意の Delta catalog を空の catalog へ適用しても panic しない
fuzz_target!(|data: &[u8]| {
    if let Ok(MsfCatalogDocument::Delta(delta)) = MsfCatalogDocument::decode(data) {
        let mut catalog = MsfCatalog::new();
        let _ = catalog.apply_delta(&delta, None);
        let mut with_namespace = MsfCatalog::new();
        let namespace = parse_catalog_namespace("catalog-ns")
            .expect("fuzz target の固定 namespace は v02 の制約を満たす");
        let _ = with_namespace.apply_delta(&delta, Some(&namespace));
    }
});

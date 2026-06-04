#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::msf::{MsfCatalog, MsfCatalogDocument};

// 任意の Delta catalog を空の catalog へ適用しても panic しない
fuzz_target!(|data: &[u8]| {
    if let Ok(MsfCatalogDocument::Delta(delta)) = MsfCatalogDocument::decode(data) {
        let mut catalog = MsfCatalog::new();
        let _ = catalog.apply_delta(&delta, None);
        let mut with_namespace = MsfCatalog::new();
        let _ = with_namespace.apply_delta(&delta, Some("catalog-ns"));
    }
});

#![no_main]

use libfuzzer_sys::fuzz_target;

// CWT 形式の DPoP proof (draft-ietf-moq-c4m-02 §4.2) を任意バイト列からデコードし、
// ヘッダ / kccs (cnf の COSE_Key) / CWT Claims Set の組み立てが panic しないことを
// fuzz する。デコードは署名検証を行わないため、暗号実装を必要としない
fuzz_target!(|data: &[u8]| {
    // actx / ath の label は TBD のため、複数の設定でデコード経路を通す
    for (actx_label, ath_label) in [(-65539, -65540), (100, 101)] {
        let Ok(codec) = shiguredo_moqt::c4m::dpop::CwtDpopProofCodec::new(actx_label, ath_label)
        else {
            // 片方の label 設定で失敗しても、もう片方のデコード経路を実行する
            continue;
        };
        let _ = codec.decode(data);
    }
});

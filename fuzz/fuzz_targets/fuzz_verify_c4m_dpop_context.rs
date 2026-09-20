#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // 暗号を必要としない DPoP proof の検証 (actx の整合) を fuzz する
    if let Ok(text) = core::str::from_utf8(data)
        && let Ok(proof) = shiguredo_moqt::c4m::dpop::DpopProof::decode(text)
    {
        let context = &proof.claims().authorization_context;
        let _ = context.verify_resource_consistency();
        let _ =
            context.verify_context_type(shiguredo_moqt::c4m::dpop::MOQT_AUTHORIZATION_CONTEXT_TYPE);
        let _ = context.verify_action(shiguredo_moqt::c4m::MoqtAction::Publish);
        // 生 JSON はアクセス可能なことだけ確認する
        assert!(!context.raw.is_empty());
    }
});

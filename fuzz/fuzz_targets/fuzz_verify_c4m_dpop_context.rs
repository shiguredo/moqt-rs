#![no_main]

use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::c4m::MoqtAction;
use shiguredo_moqt::c4m::dpop::{MOQT_AUTHORIZATION_CONTEXT_TYPE, DpopProof};

fuzz_target!(|data: &[u8]| {
    // 暗号を必要としない DPoP proof の検証 (actx の整合) を fuzz する
    if let Ok(text) = core::str::from_utf8(data)
        && let Ok(proof) = DpopProof::decode(text)
    {
        let context = &proof.claims().authorization_context;
        // 検証対象のアクションは入力の末尾バイトで `MoqtAction::ALL` から選ぶ。
        // 有効な JWT の先頭バイトはヘッダの `e` に固定されるため、先頭バイトでは
        // Setup / track の分岐に到達しない
        let action = MoqtAction::ALL
            [usize::from(data.last().copied().unwrap_or(0)) % MoqtAction::ALL.len()];
        let _ = context.verify_resource_consistency(action);
        let _ = context.verify_context_type(MOQT_AUTHORIZATION_CONTEXT_TYPE);
        let _ = context.verify_action(MoqtAction::Publish);
        // 生 JSON はアクセス可能なことだけ確認する
        assert!(!context.raw.is_empty());
    }
});

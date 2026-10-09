#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::c4m::MoqtAction;
use shiguredo_moqt::c4m::dpop::{AuthorizationContext, MOQT_AUTHORIZATION_CONTEXT_TYPE};
use shiguredo_moqt::message::common::TrackNamespace;

/// fuzz 入力
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// Authorization Context の JSON 候補
    context: Vec<u8>,
    /// `MoqtAction::ALL` の選択に使うバイト
    action: u8,
    /// `verify_target` に渡す Track Namespace のフィールド列
    namespace: Vec<Vec<u8>>,
    /// `verify_target` に渡す Track Name
    track_name: Vec<u8>,
}

// 任意の Authorization Context (actx) をデコードし、暗号を必要としない検証 API
// (type / action / target / resource) が panic しないことを fuzz する
fuzz_target!(|input: FuzzInput| {
    // actx は JSON テキストのため、UTF-8 として解釈できるときだけ渡す
    let Ok(text) = core::str::from_utf8(&input.context) else {
        return;
    };
    let Ok(context) = AuthorizationContext::decode(text) else {
        return;
    };
    // 検証対象のアクションは入力バイトで `MoqtAction::ALL` から選ぶ
    let action = MoqtAction::ALL[usize::from(input.action) % MoqtAction::ALL.len()];
    let _ = context.verify_context_type(MOQT_AUTHORIZATION_CONTEXT_TYPE);
    let _ = context.verify_action(action);
    // Track Namespace はフィールド数 32 以下 / 空フィールド無し / 4096 バイト以下という
    // 制約があるため、組み立てに失敗した場合は target の検証を行わない
    if let Ok(namespace) = TrackNamespace::new(input.namespace) {
        let _ = context.verify_target(&namespace, &input.track_name);
    }
    let _ = context.verify_resource_consistency();
});

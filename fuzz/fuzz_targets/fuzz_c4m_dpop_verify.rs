#![no_main]

use arbitrary::Arbitrary;
use libfuzzer_sys::fuzz_target;

use shiguredo_moqt::c4m::MoqtAction;
use shiguredo_moqt::c4m::dpop::{DpopProof, DpopReplayCache};
use shiguredo_moqt::message::common::TrackNamespace;

/// fuzz 入力
#[derive(Arbitrary, Debug)]
struct FuzzInput {
    /// DPoP proof の JWT (JWS compact) 候補
    proof: Vec<u8>,
    /// `MoqtAction::ALL` の選択に使うバイト
    action: u8,
    /// `verify_authorization_context` に渡す Track Namespace のフィールド列
    namespace: Vec<Vec<u8>>,
    /// `verify_authorization_context` に渡す Track Name
    track_name: Vec<u8>,
    /// 参照時刻の生ビット
    reference_time_bits: u64,
    /// ウィンドウの生ビット
    window_bits: u64,
    /// 非有限値 (NaN / 無限大) をそのまま検証 API へ渡すかどうか
    pass_non_finite: bool,
}

/// 入力ビットから検証に渡す数値を作る
///
/// 生ビットをそのまま使うと NaN / 無限大も渡る。`pass_non_finite` が真ならそのまま
/// 渡して非有限値の拒否経路を、偽なら 0.0 に差し替えて有限値の通常経路を通す。
/// どちらの経路も入力次第で必ず到達する。
fn number_from_bits(bits: u64, pass_non_finite: bool) -> f64 {
    let value = f64::from_bits(bits);
    if value.is_finite() || pass_non_finite {
        value
    } else {
        0.0
    }
}

// 任意の DPoP proof をデコードし、暗号を必要としない検証 API (鮮度 / Authorization Context /
// jti のリプレイ保護) が panic しないことを fuzz する
//
// 署名検証と JWK サムプリントの照合は aws-lc-rs feature が必要なため呼ばない。
fuzz_target!(|input: FuzzInput| {
    // proof は JWT (JWS compact) のテキストのため、UTF-8 として解釈できるときだけ渡す
    let Ok(text) = core::str::from_utf8(&input.proof) else {
        return;
    };
    let Ok(proof) = DpopProof::decode(text) else {
        return;
    };
    // iat はデコード時に有限性が検証されている
    let issued_at = proof.claims().issued_at;
    let jti = proof.claims().jti.as_str();
    // 参照時刻とウィンドウは入力の生ビットから組み立てる
    let reference_time = number_from_bits(input.reference_time_bits, input.pass_non_finite);
    let window = number_from_bits(input.window_bits, input.pass_non_finite);
    let _ = proof.verify_freshness(reference_time, window);
    // 検証対象のアクションは入力バイトで `MoqtAction::ALL` から選ぶ
    let action = MoqtAction::ALL[usize::from(input.action) % MoqtAction::ALL.len()];
    // Track Namespace の組み立てに失敗した場合は actx の検証を行わない
    if let Ok(namespace) = TrackNamespace::new(input.namespace) {
        let _ = proof.verify_authorization_context(action, &namespace, &input.track_name);
    }
    // jti のリプレイ保護。同じ jti を 2 回記録し、重複検出 (Replayed) の経路も通す
    let mut cache = DpopReplayCache::new();
    let _ = cache.check_and_record(jti, issued_at, window, reference_time);
    let _ = cache.check_and_record(jti, issued_at, window, reference_time);
});

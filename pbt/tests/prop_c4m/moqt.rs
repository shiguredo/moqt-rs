//! `moqt` クレームのエンコード / デコードと認可判定の property テスト

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::cbor;
use shiguredo_moqt::c4m::{C4mError, MOQT_SCOPES_KEY, MoqtAction, MoqtClaim, MoqtScope};

use super::common::{ModelScope, sample_model_scope};

/// 名前空間フィールドを生成する (0 〜 4 フィールド)
fn sample_namespace(ctx: &mut noprop::TestCaseContext) -> Vec<Vec<u8>> {
    let count = noprop::sample_usize_in(ctx, 0..=4);
    (0..count).map(|_| sample_bytes(ctx, 6)).collect()
}

/// モデルから組み立てたスコープを CBOR 経由で往復させ、判定がモデルと一致する
#[test]
fn authorization_matches_model() -> noprop::TestResult {
    let allowed_seen = std::cell::Cell::new(0usize);
    let denied_seen = std::cell::Cell::new(0usize);
    let nil_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let action = MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())];
        let namespace = sample_namespace(ctx);
        let track = sample_bytes(ctx, 6);
        let model = sample_model_scope(ctx, action, &namespace, &track);
        if model.ends_with_nil {
            nil_seen.set(nil_seen.get() + 1);
        }

        // SUT: モデルからスコープを組み立て、CBOR へエンコードしてからデコードし、
        // その結果で判定する (エンコード / デコードとマッチングの両方を通す)
        let scope = model.to_scope();
        let encoded = cbor::encode(&scope.encode().expect("有効なスコープをエンコードできる"))
            .expect("CBOR をエンコードできる");
        let decoded = MoqtScope::decode(&cbor::decode(&encoded).expect("CBOR をデコードできる"))
            .expect("スコープをデコードできる");

        let namespace_fields: Vec<&[u8]> = namespace.iter().map(|field| field.as_slice()).collect();
        let actual = decoded.allows(action, &namespace_fields, &track);
        let expected = model.allows(action, &namespace_fields, &track);
        assert_eq!(
            actual, expected,
            "認可判定がモデルと一致しない: model={model:?} action={action:?} \
             namespace={namespace:?} track={track:?}"
        );
        if actual {
            allowed_seen.set(allowed_seen.get() + 1);
        } else {
            denied_seen.set(denied_seen.get() + 1);
        }
        Ok(())
    })?;
    assert!(
        allowed_seen.get() > 0,
        "認可されるケースが生成されなかった\n{runner}"
    );
    assert!(
        denied_seen.get() > 0,
        "拒否されるケースが生成されなかった\n{runner}"
    );
    assert!(
        nil_seen.get() > 0,
        "nil 付きのケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// スコープの encode -> decode が一致する
#[test]
fn scope_roundtrip() -> noprop::TestResult {
    let with_track_seen = std::cell::Cell::new(0usize);
    let without_namespace_seen = std::cell::Cell::new(0usize);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let model: ModelScope = {
            let action = MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())];
            let namespace = sample_namespace(ctx);
            let track = sample_bytes(ctx, 6);
            sample_model_scope(ctx, action, &namespace, &track)
        };
        let scope = model.to_scope();
        if scope.track.is_some() {
            with_track_seen.set(with_track_seen.get() + 1);
        }
        if scope.namespace.is_empty() {
            without_namespace_seen.set(without_namespace_seen.get() + 1);
        }
        let encoded = cbor::encode(&scope.encode().expect("有効なスコープをエンコードできる"))
            .expect("CBOR をエンコードできる");
        let decoded = MoqtScope::decode(&cbor::decode(&encoded).expect("CBOR をデコードできる"))
            .expect("スコープをデコードできる");
        assert_eq!(decoded, scope, "ラウンドトリップでスコープが変わる");
        Ok(())
    })?;
    assert!(
        with_track_seen.get() > 0,
        "トラックマッチ付きのケースが生成されなかった\n{runner}"
    );
    assert!(
        without_namespace_seen.get() > 0,
        "名前空間マッチ無しのケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// 生成した claim を CBOR map へ往復させてもスコープが変わらない
#[test]
fn claim_map_roundtrip_preserves_scopes() -> noprop::TestResult {
    // 複数スコープを持つ claim が生成されたことを確認するためのカウンタ。
    // claims set の PBT は 1 スコープの claim も通すため、このテストでは複数スコープの
    // 保持を確実に踏む。
    let multi_scope_seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let scope_count = noprop::sample_usize_in(ctx, 1..=3);
        if scope_count > 1 {
            multi_scope_seen.set(true);
        }
        let mut claim = MoqtClaim::new();
        for _ in 0..scope_count {
            let action = MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())];
            let namespace = sample_namespace(ctx);
            let track = sample_bytes(ctx, 6);
            let model = sample_model_scope(ctx, action, &namespace, &track);
            claim = claim.scope(model.to_scope());
        }

        // SUT: encode → CBOR → decode → claim へ復元し、スコープが一致すること
        let encoded = cbor::encode(&claim.encode().expect("有効な claim をエンコードできる"))
            .expect("CBOR をエンコードできる");
        let decoded = MoqtClaim::decode(&cbor::decode(&encoded).expect("CBOR をデコードできる"))
            .expect("claim をデコードできる");
        assert_eq!(decoded, claim, "ラウンドトリップで claim が変わる");
        Ok(())
    })?;
    assert!(
        multi_scope_seen.get(),
        "複数スコープを持つケースが生成されなかった\n{runner}"
    );
    Ok(())
}

/// 既知の claim map にキー 0 以外のキーを追加すると必ず拒否される (fail-closed)
#[test]
fn unknown_claim_key_is_always_rejected() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let action = MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())];
        let namespace = sample_namespace(ctx);
        let track = sample_bytes(ctx, 6);
        let model = sample_model_scope(ctx, action, &namespace, &track);
        let claim = MoqtClaim::new().scope(model.to_scope());
        let claim_value = claim.encode().expect("有効な claim をエンコードできる");
        // 正しいフィクスチャであることを先に確認する (キー 0 の値は scopes 配列)。
        assert_eq!(
            MoqtClaim::decode(&claim_value).expect("生成した claim をデコードできる"),
            claim
        );
        let scopes = claim_value
            .map_get(&cbor::Value::integer(MOQT_SCOPES_KEY))
            .expect("キー 0 に scopes 配列がある")
            .clone();
        let unknown_key = noprop::sample_u64_in(ctx, 1..=1000) as i64;

        // SUT: キー 0 の claim map に未知キーを 1 つ追加すると、decode が必ず
        // UnknownMoqtClaimKey で失敗すること (未知の制限を黙って無視しない)。
        let mutated = cbor::Value::Map(vec![
            (cbor::Value::integer(MOQT_SCOPES_KEY), scopes),
            (
                cbor::Value::integer(unknown_key),
                cbor::Value::Unsigned(noprop::sample_u64_in(ctx, 0..=100)),
            ),
        ]);
        assert_eq!(
            MoqtClaim::decode(&mutated),
            Err(C4mError::UnknownMoqtClaimKey(unknown_key)),
            "未知キーを含む claim は拒否されること"
        );
        Ok(())
    })?;
    Ok(())
}

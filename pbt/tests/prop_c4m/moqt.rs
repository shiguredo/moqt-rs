//! `moqt` クレームの認可判定の property テスト

use pbt::common::{sample_bytes, test_runner};
use shiguredo_moqt::c4m::cbor;
use shiguredo_moqt::c4m::{MoqtAction, MoqtScope};

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

use shiguredo_moqt::parameter::SetupOptions;

use pbt::common::{SETUP_SAMPLING_FULL, sample_setup_options_with, test_runner};

/// 任意 SetupOptions のエンコード → デコード ラウンドトリップ
#[test]
fn roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    // 空と非空の両方が観測されたかを数える (空はカウントプレフィックスなしの 0 バイト境界)
    let empty_seen = std::cell::Cell::new(false);
    let non_empty_seen = std::cell::Cell::new(false);
    runner.run(256, |ctx| {
        let options = sample_setup_options_with(ctx, &SETUP_SAMPLING_FULL);
        if options.is_empty() {
            empty_seen.set(true);
        } else {
            non_empty_seen.set(true);
        }
        let mut buf = Vec::new();
        options
            .encode(&mut buf)
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            SetupOptions::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, buf.len());
        // デコード後も内容が一致すること (件数のみでは値の破壊を検出できない)
        assert_eq!(decoded, options);
        Ok(())
    })?;
    assert!(
        empty_seen.get(),
        "空の SetupOptions が生成されなかった\n{runner}"
    );
    assert!(
        non_empty_seen.get(),
        "非空の SetupOptions が生成されなかった\n{runner}"
    );
    Ok(())
}

// 「非空入力なら encode 後バッファも非空」は roundtrip が成立していれば論理的に含意され、
// encode 形式 (要素を並べるだけ) からも自明なため、独立した property としては持たない。

/// sampler が MAX_CONTROL_MESSAGE_SIZE (0x09) を生成すること (roundtrip のカバレッジゲート)
///
/// `roundtrip` は sampler が生成した値だけを往復させる。0x09 は奇数型の Length 付き
/// バイト列の中身が varint という特殊な形式であり、sampler が生成しないとこの経路の
/// 往復が検証されない (draft-ietf-moq-transport-23 §9.1.8 (MAX_CONTROL_MESSAGE_SIZE))。
#[test]
fn sample_covers_max_control_message_size() -> noprop::TestResult {
    let seen = std::cell::Cell::new(false);
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let options = sample_setup_options_with(ctx, &SETUP_SAMPLING_FULL);
        if options.max_control_message_size().is_some() {
            seen.set(true);
        }
        Ok(())
    })?;
    assert!(
        seen.get(),
        "MAX_CONTROL_MESSAGE_SIZE が生成されなかった\n{runner}"
    );
    Ok(())
}

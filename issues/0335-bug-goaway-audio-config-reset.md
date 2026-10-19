# GOAWAY 移行後に Audio Config の送信・検証状態をリセットする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-goaway-audio-config-reset
- Polished: {YYYY-MM-DD}

## 目的

moq-pub は最初の音声 Object にだけ Audio Config (OpusHead / AudioSpecificConfig) を載せ、moq-sub はそれを 1 度だけ検証する。GOAWAY で新しいセッションへ移行すると、publisher と subscriber のフラグが true のまま残るため、移行後の新規購読では Audio Config が届かず、購読者は音声をデコードできない。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `audio_config_sent` は移行後も true のままである。
- `examples/moq-sub/src/pipeline.rs` の `audio_config_handled` は移行後も true のままである。
- 移行処理 (`migration_target` / `resubscribe_tracks` 周辺) でこれらのフラグをリセットしていない。
- 影響: GOAWAY 移行後の音声がデコードできない。A/V 同期の基準も失われる。

## 設計方針

- 移行成功時に publisher の `audio_config_sent`、subscriber の `audio_config_handled` を false に戻す。
- 映像の Video Config (キーフレームで再送される) と同様に、音声も新しいセッションの最初の Object に載せる。
- `peer_harness` で移行後の最初の音声 Object に Audio Config が載ることを検証する。

## 完了条件

- GOAWAY 移行後の新セッションで、最初の音声 Object に Audio Config が載ること。
- 購読側が移行後の Audio Config を検証できること。
- 上記をテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

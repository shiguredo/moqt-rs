# GOAWAY 移行後に Audio Config の送信・検証状態をリセットする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-goaway-audio-config-reset
- Polished: 2026-10-20

## 目的

moq-pub は最初にフィルタを通過して送信する音声 Object にだけ Audio Config (OpusHead / AudioSpecificConfig) を載せ、moq-sub は受信した Audio Config を 1 度だけ検証する。
GOAWAY で新しいセッションへ移行すると、publisher の `audio_config_sent` と subscriber の `audio_config_handled` が true のまま残るため、移行後の新しいセッションでは音声 Object に Audio Config が載らず、subscriber も検証しない。
Audio Config は draft-ietf-moq-loc-04 §2.3.3.1 で WebCodecs の `AudioDecoderConfig.description` に対応する構成情報であり、これをデコード構成に使う購読者は構成情報を得られない。MP4 録画も OpusHead なしの catalog 値フォールバックになる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `audio_config_sent` は移行後も true のままである。
- `examples/moq-sub/src/pipeline.rs` の `audio_config_handled` は移行後も true のままである。
- publisher の移行処理 (`establish_publisher_session` の成功後) と subscriber の移行処理 (`resubscribe_tracks` の成功後) で、これらのフラグをリセットしていない。
- `audio_config_sent` は `ObjectFilterOutcome::Pass` のときだけ true になる。Audio Config は「最初にフィルタを通過して送信される音声 Object」に載り、フィルタで Skip された場合は次の Object に載せ直す。
- 影響: 移行後の新しいセッションでは Audio Config が届かない。moq-sub の再生経路は catalog のサンプルレート / チャンネル数でデコーダを構成するため直ちにデコード不能にはならないが、Audio Config の検証が行われず、MP4 録画は OpusHead なしの catalog 値フォールバックになる。
  Audio Config をデコード構成に使う購読者は構成情報を得られない。A/V 同期の基準は LOC の Timestamp / Timescale (draft-ietf-moq-loc-04 §2.3.1.1 / §2.3.1.2) であり、Audio Config の欠落では失われない。

## 再現手順

実 relay で GOAWAY を送らせる手段は現状無いため、再現はコード経路で確認する。

1. `examples/moq-pub/src/pipeline.rs` の `run` で、最初にフィルタを通過する音声 Object を送信して `audio_config_sent` が true になった状態にする。
2. `SessionEvent::GoawayReceived` の移行成功分岐を通し、新しい session へ差し替える。
3. 移行後の次の音声 Object では `audio_config_sent` が true のままなので `audio_config` が None になり、Audio Config が載らない。
4. `examples/moq-sub/src/pipeline.rs` の `run` でも、移行後は `audio_config_handled` が true のままなので、仮に移行後に Audio Config が届いても検証されない。
5. 期待: 移行後に最初にフィルタを通過して送信される音声 Object に Audio Config が載り、subscriber がそれを検証する。

pipeline 全体の GOAWAY 移行テストは 0338 の統合テスト基盤で扱う。

## 設計方針

- 移行成功時に publisher の `audio_config_sent`、subscriber の `audio_config_handled` を false に戻す。
- publisher は `establish_publisher_session` が成功した後、新しい session へ差し替える前に `audio_config_sent = false` にする。
- subscriber は `resubscribe_tracks` が成功した後、新しい session の data plane を datagram 受信タスクへ渡す前に `audio_config_handled` を false にする。`audio_config_handled` は datagram / subgroup stream / FETCH 応答で共有しているため、リセットは全経路に反映される。
- 音声 Object はフィルタで Skip され得るため、Audio Config は「移行後に最初にフィルタを通過して送信される音声 Object」に載せる。
- 映像の Video Config はキーフレームごとに再送されるため、移行後の次のキーフレームで自然に復旧する。音声はキーフレームが無いため、明示的なリセットが必要である。
- `migration_target` が `Ok(None)` を返す request stream の GOAWAY や移行失敗では、フラグを変更しない。
- 0330 は `audio_config_sent` を購読ごとに持つ設計を扱う。本 issue のリセット対象は PUBLISH 購読のフラグであり、0330 実装後も移行時に PUBLISH 購読のフラグをリセットする必要がある。
- 移行成功時のリセット処理は、`run` の移行分岐から呼ぶ小さな関数に切り出し、`#[cfg(test)]` の単体テストで固定する。`peer_harness` は `examples/tokio-moq` の `#[cfg(test)]` モジュールであり、`examples/moq-pub` / `examples/moq-sub` からは参照できない。pipeline 全体の GOAWAY 移行テストは 0338 の統合テスト基盤で扱う (本 issue の完了条件には含めない)。

## 完了条件

- GOAWAY 移行後の新セッションで、最初にフィルタを通過して送信される音声 Object に Audio Config が載ること。
- 移行成功後に subscriber の `audio_config_handled` が false になり、移行後に受信した Audio Config が検証対象になること。
- リセット処理が `#[cfg(test)]` の単体テストで固定されていること。pipeline 全体の GOAWAY 移行テストは 0338 の統合テスト基盤で扱う (本 issue の完了条件には含めない)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

# moq-sub が購読の確立前に FIN まで届いた Subgroup ストリームを捨てない

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscriber-pending-fin-stream-discard
- Polished: {YYYY-MM-DD}

## 目的

moq-sub は Track Alias が未確立の Subgroup ストリームを短時間保持する (draft-ietf-moq-transport-23 §3.1.3.1 の "buffer it briefly")。
しかし保持中に自ストリームの FIN が届くと、保持したチャンクがすべて揃っているのに abort して捨てる。そのため SUBSCRIBE_OK より先に FIN まで届いた短い Subgroup は、直後に購読が確立しても失われる。保持の目的 (制御メッセージとの順序入れ替わりへの対処) を FIN 済みのストリームでも果たす。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `register_subgroup_header` は、保持中に `receive_registered_stream_data` が `None` (FIN / RESET_STREAM / Session の STOP_SENDING 指示) を返すと `note_end_of_stream(id)` で自分の entry に終端を通知する。
  次のループで `take_ready_for` が `PendingNotifyReason::EndOfStream` を返すと、debug ログを出して `abort_subgroup_stream` を呼び、`false` を返す (保持したチャンクは decoder へ渡らない)。
- `src/pending_subgroup_buffer.rs` の `note_end_of_stream` の doc は「保持しているチャンクは捨てない (所有者が読み直せるように残す)」とし、捨てるかどうかは所有者 (example) の選択である。
- moq-pub の音声は 1 フレームごとに `SubgroupWriter::new` → `write_object` → `finish` で 1 本のストリームを使うため、音声のストリームは短く、FIN まで一度に届きやすい。
- 移植元の moqt-js も FIN の時点で subscriber が居なければ entry を破棄する (`src/session/dataStreamIncoming.ts`)。本 issue はこの挙動を変える。

## 設計方針

- 保持中に終端が FIN の場合は abort せず、保持したチャンクを手元に残して購読の確立を待ち続ける (期限は保持の開始時刻からの `timeout_us` のまま)。
  確立を検出したら (`recv_subgroup_header` の再試行が `Accepted`)、チャンクを decoder へ積み直して FIN までの Object を処理する。期限までに確立しなければ従来どおり破棄する。
- RESET_STREAM と Session の STOP_SENDING 指示による終端は、ストリームの内容が欠けている可能性があるため従来どおり abort する。`receive_registered_stream_data` の戻り値は FIN と他の終端を区別しないため、区別できる形にする。
- 終端後も待ち続ける間はストリームを読まない (読む内容が無い) ため、待機は期限付きの再試行で行う。
- moqt-js との挙動の違いは pipeline の doc に理由とともに書く。

## 完了条件

- 購読の確立前に FIN まで届いた Subgroup ストリームが、期限内に購読が確立すれば decoder へ届けられること。
- RESET_STREAM / STOP_SENDING 指示による終端と、期限切れでは従来どおり破棄されること。
- 変更はコード上で確認する (`crates/moq-sub/src/pipeline.rs` の `mod tests` には `transport::RecvStream` と `DataPlaneHandle` を組み立てて保留経路を駆動する基盤が無く、モック・スタブも使えないため)。純関数に切り出せる判定は単体テストで固定する。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

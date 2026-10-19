# moq-sub が PUBLISH_DONE を status code と購読単位で扱う

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscriber-publish-done-handling
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE) と §12.4 (Publish Done Codes) は、PUBLISH_DONE がその subscription の終端であり、status code ごとに意味が異なることを定める。moq-sub は PUBLISH_DONE を 1 通受けると status code も track も問わずパイプライン全体を停止するため、音声だけ Track Ended になっても映像が止まる。

## 現状

- `examples/moq-sub/src/pipeline.rs` のメインループは `SessionEvent::PublishDoneReceived` を受けると `Ok(true)` を返し、`peer_ended` としてパイプライン全体を終了させる。
- `status_code` / `stream_count` はログに出すだけで、終端判定や subscription ごとの終了に使っていない。
- §9.9 は受信側に SUBGROUP_DELIVERY_TIMEOUT / OBJECT_DELIVERY_TIMEOUT 以上のタイマー設定と、状態破棄時の STOP_SENDING を SHOULD とする。stream_count による終端確定も行っていない。
- 影響: 1 track の終了が全体停止になる。MSF §11.3 の Track Ended で音声だけ終了するケースで映像が止まる。

## 設計方針

- status code 別に扱う。TRACK_ENDED は該当 track のみ終了、GOING_AWAY はセッション移行、その他はエラーとして扱う。
- subscription ごとに PUBLISH_DONE 受信後の drain タイマーと stream_count の突き合わせを行い、該当購読だけを終了する。
- テストで status code 別の挙動を固定する。

## 完了条件

- 1 track の PUBLISH_DONE で他の track が止まらないこと。
- stream_count と open stream 数に基づく終端確定が実装されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

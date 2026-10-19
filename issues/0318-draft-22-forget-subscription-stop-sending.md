# forget_subscription 時に関連 stream へ STOP_SENDING を指示する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-forget-subscription-stop-sending
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE) は "The subscriber SHOULD send STOP_SENDING on all streams related to the subscription when it deletes subscription state." と SHOULD を課す。`forget_subscription` は購読状態を削除するが、open な受信 stream への STOP_SENDING を指示せず、doc にもアプリ責務の記載が無い。SHOULD からの説明なき逸脱になっている。

## 現状

- `src/session/subscription.rs` の `forget_subscription` は、関連する受信 stream を `remove_incoming_data_streams_for_request` で取り除き、破棄対象 ID の保持集合へ移すだけである。
- `SessionEvent::StopSendingDataStream` は GOAWAY drain の経路でしか発行されない。
- 影響: 購読状態を消した後も transport 上の stream が残り、アプリが自前で STOP_SENDING しない限り受信が続く。doc にもその責務が書かれていない。

## 設計方針

- `forget_subscription` の時点で、除去対象の open な受信 data stream に `StopSendingDataStream` を発行する。
- 二重発行を避けるため、既に終端済み・破棄対象の stream は対象外にする。
- API 契約としてアプリに委ねる場合は、doc に明記しテストで固定する。

## 完了条件

- `forget_subscription` で open な関連受信 stream に `StopSendingDataStream` が発行されること (または明示的な代替契約が doc とテストで固定されていること)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

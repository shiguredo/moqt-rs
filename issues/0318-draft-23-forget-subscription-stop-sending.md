# forget_subscription 時に関連する受信 data stream へ STOP_SENDING を指示する

- Created: 2026-10-19
- Completed: 2026-11-11
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)
- Polished: 2026-10-20
- Refreshed: 2026-11-04

## 目的

draft-ietf-moq-transport-23 §9.9 (PUBLISH_DONE) は "The subscriber SHOULD send STOP_SENDING on all streams related to the subscription when it deletes subscription state." と SHOULD を課す。`Session::forget_subscription` は購読状態を削除するが、open な受信 data stream への STOP_SENDING を指示せず、doc にもアプリ責務の記載が無い。
SHOULD からの説明なき逸脱になっている。

本 issue は subscriber 役の受信 data stream を対象とする。publisher 役の open な送信 stream の reset は 0308 の範囲であり、本 issue では扱わない。

## 現状

- `src/session/subscription.rs` の `Session::forget_subscription` は、`remove_incoming_data_streams_for_request` で関連する受信 data stream を処理し、移管されないものを除去する。`SessionEvent::StopSendingDataStream` は発行しない。
- `Subscription::cleanup_ready` はキャンセル由来 `Terminated` (`publish_done` が `None`) で即 true になるため、open な受信 stream が残ったまま `Session::forget_subscription` を呼べる (`src/session/data.rs` の `remove_incoming_data_streams_for_request` doc も同様)。
- `remove_incoming_data_streams_for_request` は、共有 Track Alias で最後に Object を受理した帰属先または同じ Track Alias の生きた他候補がある subgroup stream を移管して残す。移管されない subgroup stream と、`request_id` が一致する `IncomingDataStream::Fetch` は除去される。fill fetch stream か通常の FETCH 応答 stream かは `fetches` の entry の有無で判別する。
- 除去した fill fetch stream の ID は、`Session::tolerates_late_data_stream` が true のときだけ保持集合へ移される。通常の cancel 由来 `Terminated` の `forget_subscription` では保持されず、その後に peer が FIN / RESET_STREAM で閉じると unknown stream id の `SESSION_PROTOCOL_VIOLATION` になる。
- `SessionEvent::StopSendingDataStream` は `Session::tick` の DATA_STREAM_TIMEOUT 経路 (`src/session/goaway.rs`) と、subscriber 役 subscription への REQUEST_ERROR 送信での終端経路 (`src/session/data.rs` の `discard_rejected_subscription_incoming_streams`) で発行される。`src/session/types.rs` の `StopSendingDataStream` doc と `skills/shiguredo-moqt/SKILL.md` の `SessionEvent` 表もこの 2 経路を記載している。`forget_subscription` は発行しない。
- 影響: 購読状態を消した後も transport 上の stream が残り、アプリが自前で STOP_SENDING しない限り受信が続く。doc にもその責務が書かれていない。fill fetch stream では遅延終端がセッションエラーになる。

## 設計方針

- `Session::forget_subscription` の時点で、実際に除去する open な受信 data stream のうち対象となるものごとに `SessionEvent::StopSendingDataStream { stream_id, error_code: CANCELLED (0x1) }` を発行する。アプリ契約に委ねる案は、現行の SHOULD 逸脱を説明できないまま残すため採らない。
- 対象は次に限る。
  - `IncomingDataStream::Subgroup` のうち `request_id` が忘却対象と一致し、移管先の生きた subscription が無いもの。
  - `IncomingDataStream::Fetch` のうち `request_id` が忘却対象と一致し、`fetches` に entry を持たない fill fetch stream。
- 次は対象外とする。
  - 共有 Track Alias で別の生きた subscription へ移管される subgroup stream (移管先の受信を止めてしまうため)。
  - `IncomingDataStream::Discarded` と既に終端済みの stream (既に破棄対象として扱われているため、STOP_SENDING を重ねて発行しない)。
  - `fetches` に entry を持つ通常の FETCH 応答 stream (request id の一意性により subscription の request id と一致しない前提)。
  - bidi request stream (既存の `Session::stop_sending` と I/O 層の責務)。
- イベント発行は `Session::forget_subscription` の経路に限定する。`remove_incoming_data_streams_for_request` は `Session::forget_fetch` / `terminate_subscriber_fetch` / `fetch_stop_sending_received` からも呼ばれるため、ヘルパ内で無条件に発行すると FETCH の挙動 (0303 の範囲) を変えてしまう。ヘルパが除去した stream ID を返すか、購読用のフラグを渡して呼び出し側で発行する。
- 除去した fill fetch stream の ID は破棄対象 ID の保持集合 (`data_streams.discarded`) へ移す。STOP_SENDING 後に peer が送る FIN / RESET_STREAM を no-op で吸収するためである。`forget_subscription` 経路では `tolerates_late_data_stream` の判定に依存せず、`remove_incoming_data_streams_for_request` 側で保持する。
- 複数の stream を除去する場合は `stream_id` の昇順でイベントを発行し、出力順を決定的にする。
- 次の doc を新挙動に合わせて更新する。
  - `src/session/types.rs` の `SessionEvent::StopSendingDataStream` doc: 発行経路に `Session::forget_subscription` を追加し、対象 stream と保持集合への移動を明記する。
  - `src/session/subscription.rs` の `Session::forget_subscription` doc: 除去する open な受信 data stream に STOP_SENDING を発行すること、移管する stream は対象外であることを明記する。
  - `src/session/data.rs` の `remove_incoming_data_streams_for_request` doc: 除去した fill fetch stream の ID を保持集合へ移すことを明記する。
  - `skills/shiguredo-moqt/SKILL.md` の `SessionEvent` 表と forget API の節: `StopSendingDataStream` の発行経路に `forget_subscription` を追加する。
  - `crates/tokio-moq/src/moqt_client.rs` と `crates/moq-sub/src/pipeline.rs` の `StopSendingDataStream` を扱うコメント / ログ: DATA_STREAM_TIMEOUT 固定の記述を、timeout と `forget_subscription` の両経路を表す記述に直す。

## 完了条件

- subscriber が `Session::stop_sending` で購読を Terminated にした後、open な受信 subgroup stream を残したまま `Session::forget_subscription` を呼ぶと、当該 stream ごとに `StopSendingDataStream { error_code: CANCELLED }` が 1 回発行されること。
- open な fill fetch stream を残したまま `Session::forget_subscription` を呼んだ場合も、同様に `StopSendingDataStream` が発行されること。
- 共有 Track Alias で別の生きた subscription へ移管される subgroup stream には `StopSendingDataStream` が発行されないこと。
- `IncomingDataStream::Discarded` の stream と、既に終端済みの stream には `StopSendingDataStream` が発行されないこと。
- `Session::forget_subscription` で除去した fill fetch stream の ID が保持集合へ移り、STOP_SENDING 後に届く FIN / RESET_STREAM が `SESSION_PROTOCOL_VIOLATION` にならず no-op で吸収されること。
- `Session::forget_fetch` / `terminate_subscriber_fetch` / `fetch_stop_sending_received` の経路で `StopSendingDataStream` が新たに発行されないこと。
- 上記の doc (`src/session/types.rs` / `src/session/subscription.rs` / `src/session/data.rs` / `skills/shiguredo-moqt/SKILL.md` / crates のコメント / ログ) が新挙動を反映していること。
- 上記を `tests/test_session/subscription/` と `tests/test_session/fetch/` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/session/data.rs` の `remove_incoming_data_streams_for_request` が、除去した stream の内訳を `RemovedIncomingStreams` (`subgroup_ids` / `fetch_ids`、どちらも昇順) で返すようにした。`Discarded` の stream は別のベクタに分け、`subgroup_ids` に混ぜない。除去した FETCH data stream の id は `tolerates_late_data_stream` の判定に依存せず破棄対象の保持集合へ移すようにした。
- `src/session/subscription.rs` の `Session::forget_subscription` が、除去した open な受信 data stream に `SessionEvent::StopSendingDataStream { stream_id, error_code: STREAM_CANCELLED }` を stream_id の昇順で発行するようにした。対象は移管先の無い subgroup stream と、`fetches` に entry を持たない fill fetch stream である。移管した subgroup stream、通常の FETCH 応答 stream、`Discarded` と終端済みの stream は対象外である。イベント発行は `forget_subscription` の経路に限定し、`forget_fetch` / `terminate_subscriber_fetch` / `fetch_stop_sending_received` は発行しない。
- doc を更新した。
  - `src/session/types.rs` の `SessionEvent::StopSendingDataStream`: 発行経路に `forget_subscription` を追加し、対象 stream・保持集合への移動・昇順発行を明記した。
  - `src/session/subscription.rs` の `Session::forget_subscription`: 除去する open な受信 data stream に STOP_SENDING を発行すること、移管する stream は対象外であることを明記した。
  - `src/session/data.rs` の `remove_incoming_data_streams_for_request`: 戻り値と fill fetch stream の保持集合への移動を明記した。
  - `skills/shiguredo-moqt/SKILL.md` の `SessionEvent` 表と forget API の節に `forget_subscription` 経路を追加した。
  - `crates/tokio-moq/src/moqt_client.rs` と `crates/moq-sub/src/pipeline.rs` の `StopSendingDataStream` を扱うコメントを、timeout 固定の記述から timeout / REQUEST_ERROR / 購読削除の 3 経路を表す記述に直した。
- テスト: `tests/test_session/subscription/forget_stop_sending.rs` を追加した。
  - `forget_subscription_emits_stop_sending_for_open_subgroup_stream`: open な subgroup stream を残したまま forget すると `StopSendingDataStream { CANCELLED }` が 1 回発行され、その後の FIN が no-op で吸収されること。
  - `forget_subscription_emits_stop_sending_for_fill_fetch_stream`: open な fill fetch stream でも発行され、終端通知が吸収されること。
  - `forget_subscription_does_not_emit_stop_sending_for_migrated_stream`: 共有 Track Alias で生きた別 subscription へ移管される stream には発行されないこと。
  - `forget_subscription_does_not_emit_stop_sending_for_discarded_stream`: `Discarded` の stream には発行されないこと。
  - `fetch_cancel_does_not_emit_stop_sending_data_stream`: FETCH の cancel (`send_fetch_stop_sending`) では `StopSendingDataStream` を発行しないこと。
- issue の「現状」にあった「`StopSendingDataStream` は DATA_STREAM_TIMEOUT 経路でしか発行されない」は、subscriber 役 subscription への REQUEST_ERROR 送信経路でも発行されるため、実態に同期した。
- 検証: `cargo test -p shiguredo_moqt --test test_session` (1120 件) / `cargo test -p shiguredo_moqt --lib` (133 件) / `cargo clippy -p shiguredo_moqt -p tokio-moq -p moq-sub --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通る。

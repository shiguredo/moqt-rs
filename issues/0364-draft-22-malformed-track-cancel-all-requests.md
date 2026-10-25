# Malformed Track 検出時に同じ Track の全 subscription / FETCH を cancel する

- Created: 2026-10-25
- Completed: 2026-10-25
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) は "When a subscriber detects a Malformed Track, it MUST cancel any corresponding
subscription or fetches for that Track from that publisher" と MUST を課す。draft-22 は同一 Track への複数同時 subscription を許可しており、
1 本の subscription で malformed を検出したとき、同じ Track に対する他の subscription / FETCH を残すとこの MUST を満たせない。
現状は検出した request だけを終端している。

## 現状

- `src/session/data.rs` の `Session::terminate_malformed_track` は引数の `request_id` に対応する 1 件だけを `Terminated` にし、
  `SessionEvent::StopSendingRequestStream` → `ResetRequestStream` → `RequestTerminated { reason: MalformedTrack }` を発行する。
  doc コメントにも「他の request は生き残る」と書かれている。
- 同関数は既に `Terminated` の request に対する早期 return を持つ。キャンセル由来 (`publish_done` が `None`) はイベントを発行せず、
  PUBLISH_DONE 受信済み (drain 中、`publish_done` が `Some`) は `publish_done` を `None` に戻して破棄対象へ移し、`ResetDataStream` だけを発行する。
- `src/session/subscription.rs` の `Session::register_subscription` は `AliasState::subscriptions_by_track` を
  (Track Namespace, Track Name, TrackRole) キーで維持しており、同一 Track の subscription を列挙できる。索引からの除去は
  `forget_subscription` と `supersede_pending_subscriber` だけで、cancel や FIN 終端で `Terminated` になっただけの subscription は残る。
- `src/session/types.rs` の `Fetch` は publisher 役 / subscriber 役とも `track_namespace` / `track_name` を保持する。
  `terminate_malformed_track` には fetch の終端分岐が既にあるが、現時点では subscription 由来の呼び出ししかなく到達不能と明記されている。
- 同一 Track への複数同時 subscription の受理は `tests/test_session/subscription/handshake.rs` で、Track Alias 共有は
  `tests/test_session/shared_track_alias.rs` で許可済みである。
- `src/session/tests.rs` の `malformed_object_after_fin_terminates_attributed_subscription` と
  `malformed_mark_fin_failure_terminates_attributed_subscription` は、同一 Track の 2 本の subscription のうち片方だけが終端され、
  もう片方が `Established` のまま残る現行挙動を固定している。本 issue の変更で両テストの期待値は変わる。
  前者は open な data stream 上の Object 受信で検出するため `stream_id` は `Some`、後者は `recv_data_stream_closed` の FIN で検出するため
  `stream_id` は `None` になる経路である。
- `tests/test_session/data_stream.rs` の既存 malformed テストは別 Track の 2 本を対象にしており、同一 Track の複数 request を対象にしたテストは無い。

## 設計方針

- malformed を検出した request の Track Namespace / Track Name を使い、同じ Track に対する自側 subscriber 役の subscription と FETCH を列挙する。
  publisher 役の request は malformed 検出の対象ではないため対象外とする。
- 列挙は `AliasState::subscriptions_by_track` と `Session.fetches` を使い、検出した request 自身は列挙結果から除外する。
  検出した request は既存の `terminate_malformed_track(request_id, stream_id, reason)` に、検出経路が渡す `Option<DataStreamId>` を
  そのまま渡して最後に処理する (open な data stream 上の検出時は `Some`、FIN 終端や datagram の検出時は `None`)。
- 検出した request 以外の各 request は、状態に応じて次のように処理する。`reason` は `MalformedTrack`、STOP_SENDING / RESET_STREAM の
  `error_code` は既存の malformed 終端と同じ `STREAM_MALFORMED_TRACK` (0x12) とする。
  - 非 `Terminated`: `Terminated` へ遷移させ、bidi request stream の受信方向 STOP_SENDING と送信方向 RESET_STREAM
    (§6.4.2.3 (Request Cancellation and Rejection) の cancel 手順)、`RequestTerminated` を 1 回ずつ発行する。
  - drain 中 (`Terminated` かつ `publish_done` が `Some`): `publish_done` を `None` に戻して以後の data stream を破棄対象に移す。
    `RequestTerminated` と request stream の cancel は再発行しない (既存の二重終端抑止と同じ扱い)。
  - キャンセル由来 (`Terminated` かつ `publish_done` が `None`): 状態を変えず no-op とする。
- 対象 request の data stream を reset する必要はない。request stream の STOP_SENDING を受けた publisher は §3.1.2 (Subscription State Management) /
  §3.2.4 (Fetch State Management) の MUST により関連 stream を reset する。検出した request の data stream の reset は既存どおり行う。
- `terminate_malformed_track` の doc から「他の request は生き残る」を削除し、同一 Track の全 request を cancel する動作に更新する。
- 対象 request ID を先に収集してから 1 件ずつ処理し、借用の衝突を避ける。

## 完了条件

- 同一 Track への 2 本の subscription があり、open な data stream 上の Object 受信で malformed を検出したとき、両方が `Terminated` になり、
  検出した request には `ResetDataStream` が 1 回、もう片方には `error_code` が `STREAM_MALFORMED_TRACK` (0x12) の
  STOP_SENDING / RESET_STREAM と `RequestTerminated` が 1 回ずつ発行されること。
- 同じ 2 本の subscription で、`recv_data_stream_closed` の FIN で malformed を検出したときは、検出した request に
  `ResetDataStream` が発行されないこと (既存挙動の維持)。`src/session/tests.rs` の既存 2 テストは、この 2 経路の違いを踏まえて
  期待値を新しい挙動に更新すること。
- 同一 Track への subscription と FETCH が併存するとき、FETCH も終端されること。別 Track の request と publisher 役の request は影響を受けないこと。
- drain 中の sibling が malformed 検出時に `RequestTerminated` を重複発行されず、以後の data stream が破棄対象になること。
- 同一 Track の複数 subscription、subscription と FETCH、別 Track / publisher 役の非影響、drain 中 sibling をそれぞれ検証する
  新しいテストを `tests/test_session/data_stream.rs` に追加し、sibling の request stream cancel の `error_code` が
  `STREAM_MALFORMED_TRACK` であることも検証すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/session/data.rs` の `Session::terminate_malformed_track` を、検出した request と同じ Track の全 request を
  終端するエントリポイントに変更した。検出した request の Track Namespace / Track Name と自側の役割を取得し、
  自側が subscriber 役のとき `AliasState::subscriptions_by_track` と `Session::fetches` から、同じ Track に対する
  自側 subscriber 役の subscription と FETCH を sibling として列挙する。検出した request 自身は sibling から除外し、
  最後に処理する。sibling の request id は昇順にソートし、HashMap の反復順に依存しない。
- 1 件分の終端処理を `Session::terminate_request_for_malformed_track` に分離した。既存の二重終端抑止、drain 解除
  (`publish_done` を `None` に戻す)、`StopSendingRequestStream` → `ResetRequestStream` → `RequestTerminated` の
  発行は維持する。sibling には `stream_id` を渡さない (request stream の STOP_SENDING を受けた publisher が
  draft §3.1.2 / §3.2.4 の MUST で data stream を reset するため)。
- drain 中 (`publish_done` が `Some`) の sibling は `publish_done` を `None` に戻して以後のデータを破棄対象に移し、
  `RequestTerminated` を再発行しない。あわせて `rejected_request_ids` に記録し、PUBLISH_DONE より後に開かれる
  遅延 fill fetch stream の受理と、アプリが `forget_subscription` した後に届く bidi request stream の close 通知の
  no-op 吸収を維持する。記録の寿命は `src/session/core.rs` のフィールド doc に実装どおりに明記した。
- doc コメントを新挙動に更新した。`terminate_malformed_track` / `terminate_request_for_malformed_track` から
  「他の request は生き残る」「該当 subscription だけを cancel する」を削除し、FETCH 分岐の「到達不能」記述を
  sibling 列挙から到達する旨に直した。drain 中の早期 return 経路では §12.1 の SHOULD
  "deliver an error to the application" のエラー通知を行わないことも明記した。
- `src/session/tests.rs` の既存 2 テストを新挙動に更新した。open な data stream 上の Object 受信で検出する経路は
  `ResetDataStream` が 1 回、`recv_data_stream_closed` の FIN で検出する経路は 0 回であること、request ごとに
  `StopSendingRequestStream` → `ResetRequestStream` の順で 1 回ずつ、`RequestTerminated` が 1 回ずつ発行される
  ことを検証する。
- `tests/test_session/data_stream.rs` に、同一 Track の複数 subscription、subscription と FETCH の併存、
  別 Track / publisher 役の非影響 (subscription と FETCH の両方)、drain 中 sibling、drain 解除後の遅延 datagram /
  遅延 fill fetch stream、FIN 済み FETCH の早期 return を検証するテストを追加した。
- `tests/test_session.rs` の malformed イベント回収ヘルパを request_id ごとに集計する形に拡張し、cancel 順序、
  `ResetDataStream` の error code と対象 stream、対象外 request のイベント不在を検証するようにした。
- `pbt/tests/prop_session/subgroup.rs` と `tests/test_session/datagram_object_filter.rs` のメッセージを、
  1 subscription 前提の検証内容に合わせて修正した。

## 完了条件の検証

- 同一 Track の 2 subscription で open stream 検出時に両方が `Terminated`、検出 request に `ResetDataStream` 1 回、
  sibling に `STREAM_MALFORMED_TRACK` の cancel と `RequestTerminated` 1 回ずつ:
  `malformed_track_cancels_all_subscriptions_on_same_track`
- FIN 検出時に `ResetDataStream` 0 回、request ごとの cancel と終端:
  `malformed_mark_fin_failure_terminates_all_track_subscriptions`
- subscription と FETCH の併存時に FETCH も終端されること: `malformed_track_cancels_same_track_fetch`
- 別 Track / publisher 役の subscription と FETCH が影響を受けないこと:
  `malformed_track_keeps_other_track_and_publisher_role_requests` /
  `malformed_track_keeps_other_track_and_publisher_role_fetches`
- drain 中 sibling の重複終端抑止と破棄移行:
  `malformed_track_clears_draining_sibling_without_duplicate_termination` /
  `malformed_track_drain_cleared_sibling_tolerates_late_fill_after_close`
- FIN 済み FETCH の早期 return: `malformed_track_does_not_cancel_data_stream_finished_fetch`
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` 成功

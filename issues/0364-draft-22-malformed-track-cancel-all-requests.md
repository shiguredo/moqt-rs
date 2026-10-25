# Malformed Track 検出時に同じ Track の全 subscription / FETCH を cancel する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
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

# MoqtClient::fetch の REQUEST_ERROR をアプリへ配送する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-request-error-event
- Polished: 2026-10-20

## 目的

`examples/tokio-moq/src/moqt_client.rs` の `MoqtClient::fetch` の doc は、応答が
`ClientEvent::Session` の `SessionEvent::FetchOkReceived` または
`SessionEvent::RequestErrorReceived` として届くと書く。しかし `RequestErrorReceived` は
`is_notable_event` に含まれず、`drain_events` も `request_errors` に記録するだけで
配送キューへ積まない。このため FETCH が REQUEST_ERROR で拒否されてもアプリは
`next_event` で観測できず、再試行・代替購読・REDIRECT の判断ができない。
doc が約束する配送を実装し、doc と実装を一致させる。

## 現状

- `MoqtClient::fetch` の doc は、応答が `ClientEvent::Session` の
  `SessionEvent::FetchOkReceived` (End Location / End of Track) または
  `SessionEvent::RequestErrorReceived` として届くと書く。
- `is_notable_event` は `GoawayReceived` / `CloseSession` / `PublishDoneReceived` /
  `ResetDataStream` / `OpenFillFetchStream` / `FetchOkReceived` だけを notable とし、
  `RequestErrorReceived` を含まない。
- `drain_events` の `RequestErrorReceived` arm は
  `self.request_errors.insert(request_id, (error_code, reason.as_str().to_string()))` を
  行うだけで、`notable_events` へ積まない。このため `RequestErrorReceived` が持つ
  `retry_interval` と `redirect` はアプリへ届かない。
- Session 層は FETCH の REQUEST_ERROR 受信時に `SessionEvent::RequestErrorReceived` を
  発行済みである (`src/session/core.rs` の `handle_peer_request_error` と
  `src/session/fetch.rs` の `handle_err_for_fetch`)。欠けているのは example の配送だけであり、
  `src/session/` の変更は不要である。
- `examples/moq-sub/src/pipeline.rs` の `fetch_recent_groups` は `MoqtClient::fetch` を
  呼ぶ唯一の example の実運用経路であり、「(`SessionEvent::RequestErrorReceived` は notable
  イベントではない) ためログに残せない」と doc に書いている。配送を実装するとこの記述は
  実態と合わなくなる。
- `examples/tokio-moq/src/peer_harness.rs` の
  `rejected_fetch_is_not_delivered_to_the_application` は、peer 起点の FETCH を
  Session が INVALID_RANGE で拒否したときに `ClientEvent::Request` をアプリへ
  渡さないことを固定するテストである。これは本 issue の「自側が発行した FETCH への
  REQUEST_ERROR の配送」とは別方向・別事象であり、変更してはならない。
  example の `next_event` 配送を固定するテストは現状存在しない。
- `next_event` の doc の notable イベント一覧は `FetchOkReceived` /
  `OpenFillFetchStream` も漏れている。`RequestErrorReceived` を追加するなら、
  一覧を実態に合わせる必要がある。
- `docs/missing-features.md` §6.7 は「transport 層は `(error_code, reason)` だけを
  記録して `redirect` を捨てる」と書く。配送を実装すると、イベントとしては
  `redirect` がアプリへ届くため、この記述は実態と合わなくなる。

## 設計方針

- `ClientEvent::RequestError` は追加しない。`SessionEvent::RequestErrorReceived` は
  `request_id` / `error_code` / `retry_interval` / `reason` / `redirect` を既に持ち、
  `fetch` の doc も `ClientEvent::Session` の `SessionEvent::RequestErrorReceived` を
  約束している。新しい variant を作ると同じ情報を二重に持つうえ、`fetch` の doc を
  別の型へ書き換える必要があり、「doc と実装を一致させる」目的に反する。
  0272 が `FetchOkReceived` を notable に追加したのと同じ配送経路に揃える。
- `is_notable_event` に `SessionEvent::RequestErrorReceived` を追加する。
- `drain_events` の `RequestErrorReceived` arm を、`request_errors` への記録に加えて
  `notable_events` へ積む notable 配送にする。`is_notable_event` との一致は既存の
  慣習どおり arm 内の `debug_assert!` で検査する。
- `next_event` は
  `ClientEvent::Session(SessionEvent::RequestErrorReceived { request_id, error_code, retry_interval, reason, redirect })`
  を返す。`fetch` は送信に成功した場合 `Ok(request_id)` を返し、拒否はこのイベントで観測する。
  `RequestErrorReceived` は `retry_interval` と `redirect` を含むため、再試行・
  REDIRECT の判断材料が欠けない。
- 配送対象は FETCH に限らない。`RequestErrorReceived` は SUBSCRIBE / PUBLISH /
  TRACK_STATUS / REQUEST_UPDATE にも発行され、notable に追加するとこれらも
  `next_event` に届く。`subscribe_track` / `publish_track` は同期 API として
  戻り値でもエラーを返すため、同じ request の `RequestErrorReceived` が
  `next_event` でも配送されうる。この二重性は doc に明記する
  (戻り値は同期 API の失敗、イベントはセッションの通知であり、
  `retry_interval` / `redirect` はイベントにのみ載る)。
- `request_errors` への記録は現状どおり残す。`wait_subscription_resolved` が
  subscription / publish の失敗詳細を組み立てるために使う。
- `next_event` の doc の notable イベント一覧を実態に合わせ、
  `FetchOkReceived` / `OpenFillFetchStream` / `RequestErrorReceived` を追加する。
- `fetch` の doc は現行の `SessionEvent::RequestErrorReceived` のままで正しくなる。
  `retry_interval` / `redirect` も届くことを 1 文追記する。
- `examples/moq-sub/src/pipeline.rs` の `fetch_recent_groups` の doc を更新し、
  `next_event` の `RequestErrorReceived` arm で `request_id` / `error_code` /
  `retry_interval` / `reason` / `redirect` をログに残す（ログメッセージは英語）。
  FETCH の拒否であれば `media_fetch_targets` から該当 `request_id` を削除する
  (`media_fetch_targets.remove(&request_id)` の戻り値で FETCH かどうかを判定できる)。
- `examples/moq-pub/src/pipeline.rs` は FETCH を発行せず、PUBLISH の失敗は
  `publish_track` の戻り値で終了するため、本 issue の変更対象外である。
- `docs/missing-features.md` §6.7 を、イベントで `redirect` と `retry_interval` が届くが
  example はリダイレクトに追随しない、という実態に更新する。

## 変更対象

- `examples/tokio-moq/src/moqt_client.rs`
  - `is_notable_event` / `drain_events` / `next_event` の doc / `fetch` の doc /
    `#[cfg(test)]` の notable テスト
- `examples/tokio-moq/src/peer_harness.rs`
  - `TestPeer` に REQUEST_ERROR 応答用の helper を追加し、client 発 FETCH の
    REQUEST_ERROR 配送を固定するテストを追加する。
    `rejected_fetch_is_not_delivered_to_the_application` は変更しない。
- `examples/moq-sub/src/pipeline.rs`
  - `fetch_recent_groups` の doc と `next_event` の `RequestErrorReceived` arm
- `docs/missing-features.md` §6.7

## 完了条件

- 自側が発行した FETCH が peer の REQUEST_ERROR で拒否されたとき、
  `MoqtClient::next_event` が
  `ClientEvent::Session(SessionEvent::RequestErrorReceived { request_id, error_code, retry_interval, reason, redirect })`
  を返すこと。`request_id` / `error_code` / `retry_interval` / `reason` が
  peer の送った値と一致し、`redirect` は非 REDIRECT なら `None` であること。
- `examples/tokio-moq/src/moqt_client.rs` の単体テストで
  `SessionEvent::RequestErrorReceived` が `is_notable_event` であることを固定すること。
- `drain_events` の `RequestErrorReceived` arm に、`is_notable_event` との一致を検査する
  `debug_assert!` があること。
- `examples/moq-sub/src/pipeline.rs` が FETCH の拒否をログに残し、
  `media_fetch_targets` から該当 request を除去すること。
- `rejected_fetch_is_not_delivered_to_the_application` が変更されずに通ること
  (peer 起点 FETCH の拒否が `ClientEvent::Request` として渡らないこと)。
- `next_event` / `fetch` / `fetch_recent_groups` / `docs/missing-features.md` §6.7 の
  記述が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

# example の MoqtClient が PublishDoneReceived と GoawayReceived を捨てる

- Created: 2026-09-17
- Completed: 2026-09-17
- Branch: feature/fix-example-notable-event-drop
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-transport` の `MoqtClient` が、アプリへ通知すべき `SessionEvent::PublishDoneReceived` と `SessionEvent::GoawayReceived` を `drain_events/1` で消費して捨てている。そのため example (`moqt-subscriber`) は relay / publisher から届いた PUBLISH_DONE と GOAWAY を観測できない。notable イベントを失わずにアプリへ渡す。

## 現状

- `MoqtClient::next_event/0` は `take_notable_event/0` で notable イベントを取り出してから `pump_once/0` で I/O を 1 件処理する
- `pump_once/0` は受信メッセージを `Session` へ流したあと `drain_events/0` を呼ぶ
- `drain_events/0` は `session.poll_event()` を繰り返し、`PublishDoneReceived` と `GoawayReceived` を「メインループが `take_notable_event` で拾う」とコメントしたうえで消費して捨てる catch-all 節に含めている
- したがって受信メッセージを契機に生成された notable イベントは、`next_event/0` の次の `take_notable_event/0` が呼ばれる前に `drain_events/0` が消費する。`take_notable_event/0` が取り出せるのは、I/O 処理を挟まずに `Session` がイベントを積んだ場合だけになる
- 実測 (moqt-publisher / relay / moqt-subscriber の E2E): relay が下流へ PUBLISH_DONE を `ok` 付きで送信し、同じ request stream を relay のクライアントが受信できているにもかかわらず、`moqt-subscriber` は `Received PUBLISH_DONE` を出力しない (publisher の graceful shutdown 時、relay が PUBLISH_DONE を転送した直後のログ)
- `CloseSession` は `drain_events/0` が transport の close まで行うため対象外とする

## 設計方針

- `MoqtClient` に notable イベント専用のキュー (`VecDeque<SessionEvent>`) を持たせる
- `drain_events/0` は `PublishDoneReceived` / `GoawayReceived` を捨てずにこのキューへ積む
- `take_notable_event/0` は最初にこのキューを pop し、空なら従来どおり `Session` から poll する
- `Session` の公開 API は変更しない (sans-I/O の状態機械に example 都合の peek を足さない)

## 完了条件

- `drain_events/0` の後に `take_notable_event/0` を呼んでも `PublishDoneReceived` / `GoawayReceived` が取り出せること
- `pump_once/0` 経由で PUBLISH_DONE を受信した `moqt-subscriber` が `Received PUBLISH_DONE` を出力すること
- 既存の `SessionEvent` 処理 (SendControl / SendRequest / CloseSession など) の挙動が変わらないこと
- 上記を検証するテストが追加されていること

## 解決方法

`examples/moqt-transport/src/moqt_client.rs` の `MoqtClient` に notable イベント専用の
キュー (`notable_events: VecDeque<SessionEvent>`) を追加した。

- `drain_events` は `GoawayReceived` と `PublishDoneReceived` を捨てずにこのキューへ積む
- `take_notable_event` は最初にこのキューを pop し、空なら従来どおり `Session` から poll する
- アプリが観測するイベントの判定は `is_notable_event/1` に集約し、`take_notable_event` と
  `drain_events` で同じ判定を使う
- `Session` (sans-I/O の状態機械) の公開 API は変更していない

テストは `examples/moqt-transport/src/moqt_client.rs` に 3 本追加した。

- `publish_done_is_notable_event`: PUBLISH_DONE が notable であること
- `goaway_is_notable_event`: GOAWAY が notable であること
- `established_is_not_notable_event`: アプリが観測しないイベントは notable でないこと

実測では moqt-rs の publisher / subscriber と relay を繋いだ E2E で確認した。
publisher を graceful shutdown すると relay が下流へ PUBLISH_DONE を転送し、
`moqt-subscriber` が `Received PUBLISH_DONE (status_code=4, stream_count=18446744073709551615)`
を出力して正常に終了する (修正前は PUBLISH_DONE が届いても何も出力せず、window を閉じるまで
待ち続けていた)。

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` / `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` の通過で確認した。

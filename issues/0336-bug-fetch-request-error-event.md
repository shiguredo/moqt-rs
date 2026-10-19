# MoqtClient::fetch の REQUEST_ERROR をアプリへ配送する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-request-error-event
- Polished: {YYYY-MM-DD}

## 目的

`examples/tokio-moq/src/moqt_client.rs` の `fetch` の doc は、応答が `SessionEvent::FetchOkReceived` または `SessionEvent::RequestErrorReceived` として届くと書く。しかし `RequestErrorReceived` は notable event として扱われず、アプリへ配送されない。FETCH が拒否されたことをアプリが観測できないため、doc と実装を一致させる。

## 現状

- `fetch` の doc は `RequestErrorReceived` が届くと案内している。
- `is_notable_event` は `GoawayReceived` / `CloseSession` / `PublishDoneReceived` / `ResetDataStream` / `OpenFillFetchStream` / `FetchOkReceived` だけを notable とし、`RequestErrorReceived` を含まない。
- `drain_events` は `RequestErrorReceived` を `request_errors` に記録するだけで返さない。
- `examples/tokio-moq/src/peer_harness.rs` の `rejected_fetch_is_not_delivered_to_the_application` は「届かないこと」を固定している。
- 影響: FETCH 拒否時にアプリが再試行・代替購読などの判断をできない。

## 設計方針

- `ClientEvent::RequestError` を追加してアプリへ配送する。対象 request_id と error code / reason を渡す。
- 配送を実装しない場合は `fetch` の doc を実態に合わせて「拒否は観測できない」と明記し、代替 API を案内する。
- テストを実装に合わせて更新する。

## 完了条件

- FETCH の REQUEST_ERROR をアプリが観測できること (または doc が実態と一致し、代替手段が明記されていること)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

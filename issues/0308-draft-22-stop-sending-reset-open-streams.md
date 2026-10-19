# STOP_SENDING を受けた購読の open な送信 stream を Session が reset する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-stop-sending-reset-open-streams
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §3.1.2 (Subscription State Management) は "The Publisher can remove subscription state as soon as it has received STOP_SENDING. It MUST reset any open streams associated with the SUBSCRIBE." と MUST を課す。現状は fill fetch stream だけを自動 reset し、送信中の subgroup
stream と FETCH の uni data stream はアプリが `send_data_stream_closed` / `send_fetch_data_stream_closed` を呼ぶ前提になっている。アプリが呼び忘れると MUST 違反になる。

## 現状

- `src/session/subscription/send.rs` の `close_subscription_on_stream_end` は `reset_open_fill_streams` だけを呼ぶ。
- `src/session/fetch.rs` の `send_fetch_stop_sending` は bidi request stream の reset のみで、open な FETCH data stream を reset しない。
- docs/missing-features.md §4.7 が既知ギャップとして記録しているが、未起票である (0246 は順序契約の話で、自動 reset の欠落そのものではない)。
- 影響: cancel 後に open stream が残り、Stream Count の確定と cleanup が進まない。仕様 MUST をアプリの実装漏れに依存させている。

## 設計方針

- subscription の cancel 時に、当該 request の open な送信 subgroup stream と fill fetch stream を `ResetDataStream(DELIVERY_TIMEOUT)` ではなく cancel に適切なコードで reset する。
- FETCH cancel 時に、当該 fetch の open な送信 data stream を reset する。
- 自動 reset とアプリ通知 (`send_data_stream_closed`) の二重終端を no-op で吸収する。
- Sans-I/O の制約で不可能な場合は、API 契約と doc に「アプリが必ず reset する」ことを明記し、テストで固定する。

## 完了条件

- STOP_SENDING / RESET による購読 cancel 後に、関連する open stream が Session から reset 指示されること (または代替契約が doc とテストで固定されていること)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。
- 完了時に docs/missing-features.md §4.7 を更新する。

# STOP_SENDING を受けた購読と FETCH の open な送信 stream を Session が reset する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-stop-sending-reset-open-streams
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §3.1.2 (Subscription State Management) は "The Publisher can remove subscription
state as soon as it has received STOP_SENDING. It MUST reset any open streams associated with the SUBSCRIBE."
と MUST を課す。これは SUBSCRIBE の bidi request stream に対する STOP_SENDING を指す。§3.2.4 (Fetch State Management) も "The
Publisher can remove fetch state as soon as it has received a STOP_SENDING. It MUST reset the bidi request
stream and unidirectional data stream associated with the FETCH." と MUST を課す。これは FETCH の bidi request stream
または uni data stream に対する STOP_SENDING を指す。

現状、cancel 経路で Session が自動 reset するのは fill fetch stream だけであり、publisher 役 subscription の open な subgroup stream
と、publisher 役 FETCH の open な outgoing FETCH data stream は、アプリが `send_data_stream_closed` /
`send_fetch_data_stream_closed` を呼ぶ前提になっている。アプリが呼び忘れると、publisher 側で上記 MUST を満たせない。なお `send_err_for_fetch` は
REQUEST_UPDATE 失敗応答で outgoing FETCH data stream を自動 reset しており、Session が reset 指示を発行する仕組みは既に存在する。

## 現状

- `src/session/subscription/send.rs` の `close_subscription_on_stream_end` は reset 指示としては `reset_open_fill_streams` だけを呼び、open な `data_streams.outgoing` (subgroup stream) を reset しない。
- `src/session/fetch.rs` の `fetch_stop_sending_received` は publisher 側で FETCH の uni data stream の STOP_SENDING を受けた経路だが、イベントとしては `ResetRequestStream` だけを発行し、open な `outgoing_fetch` を reset しない。
- `src/session/fetch.rs` の `close_fetch_on_stream_end` は FETCH の bidi request stream の `RequestStreamEnd::Reset` (I/O 層が STOP_SENDING をこの経路へ写像する) で `FetchState::Terminated` へ遷移させるだけで、open な `outgoing_fetch` を reset しない。
- `send_fetch_stop_sending` は subscriber 側のアプリ起点 cancel であり、subscriber は outgoing FETCH data stream を持たない。本 issue の対象は publisher 側で STOP_SENDING を受けた経路である。
- I/O 層は bidi request stream の STOP_SENDING を
  `recv_request_stream_closed(request_id, RequestStreamEnd::Reset { .. })` として Session へ渡す
  (`examples/tokio-moq/src/moqt_client.rs` の `terminate_request_stream_by_stop_sending`)。FETCH の uni data
  stream の STOP_SENDING は `fetch_stop_sending_received` が受ける (fill fetch stream は
  `recv_data_stream_stop_sending` が受ける)。
- `docs/missing-features.md` §4.7 が既知ギャップとして記録しており、本 issue で起票した。0246 は終端通知と STOP_SENDING 観測の順序契約の話であり、自動 reset の欠落そのものではない。
- 影響: cancel 後に transport 上の stream が残る。publisher は §3.1.2 / §3.2.4 の MUST をアプリの実装漏れに依存させることになる。REQUEST_UPDATE
  失敗応答で保留中の PUBLISH_DONE (UPDATE_FAILED) がある場合、open な outgoing stream が残る間は `maybe_flush_pending_publish_done`
  が flush しない。`forget_subscription` / `forget_fetch` で Session の追跡は除去できるが、transport stream 自体は閉じない。

## 設計方針

- subscription の cancel (`RequestStreamEnd::Reset`) 時、publisher 役 subscription の open な
  `data_streams.outgoing` を列挙し、各 stream に
  `SessionEvent::ResetDataStream { stream_id, error_code: STREAM_CANCELLED (0x1), reliable_size: None }`
  を発行して追跡から除去する。subgroup tracker は `reset_outgoing_data_stream_at_with_code` と同じく reset 終端へ進める。既存の
  `reset_open_fill_streams` はそのまま使い、fill fetch stream を二重に reset しない。FIN は cancel ではないため、open な subgroup
  stream の自動 reset は `RequestStreamEnd::Reset` のときだけ行う。
- FETCH の cancel 時、publisher 役 FETCH の open な `outgoing_fetch` を列挙し、各 stream に
  `ResetDataStream { stream_id, error_code: STREAM_CANCELLED, reliable_size: None }` を発行して追跡から除去し、
  `Fetch::data_stream_finished` を true にする。対象経路は bidi request stream の `RequestStreamEnd::Reset`
  (`close_fetch_on_stream_end`) と uni data stream の STOP_SENDING (`fetch_stop_sending_received`) の両方とする。
  `fetch_stop_sending_received` では既存の `ResetRequestStream` の後に `ResetDataStream` を push し、
  `close_fetch_on_stream_end` では `RequestTerminated` より前に push する。
- 本 issue の cancel 経路で自動 reset した stream id (fill fetch stream を含む) を Session 内に記録し、以後の `send_data_stream_closed` / `send_fetch_data_stream_closed` は当該 id を no-op で `Ok(())` にする (未知 id の `PROTOCOL_VIOLATION` と区別する)。記録は `forget_subscription` / `forget_fetch` で破棄する。
- Sans-I/O の制約は reset の実行を妨げない。Session は `ResetDataStream` を発行し、I/O 層が RESET_STREAM を送出する。fill fetch stream と `send_err_for_fetch` が既に同じ方式を採っているため、代替契約 (アプリが必ず reset する) は採らない。
- 関連する doc コメント (`fetch_stop_sending_received` / `send_fetch_data_stream_closed` など、アプリが reset する前提を書いている箇所) を新挙動に合わせて更新する。

## 完了条件

- publisher 役 subscription で `RequestStreamEnd::Reset` を受けたとき、当該 request の open な outgoing subgroup stream ごとに `ResetDataStream { error_code: STREAM_CANCELLED, reliable_size: None }` が発行され、追跡から除去されること。`ResetDataStream` は `RequestTerminated` より前に発行されること。既存の fill fetch stream の reset は従来どおり行われること。
- publisher 役 FETCH で bidi request stream の `RequestStreamEnd::Reset` または uni data stream の STOP_SENDING
  を受けたとき、当該 fetch の open な outgoing FETCH data stream に
  `ResetDataStream { error_code: STREAM_CANCELLED, reliable_size: None }` が発行され、追跡から除去され、
  `data_stream_finished` が true になること。`fetch_stop_sending_received` では `ResetRequestStream` の後に
  `ResetDataStream` が発行され、`close_fetch_on_stream_end` では `RequestTerminated` より前に発行されること。
- 本 issue の cancel 経路で自動 reset した stream について、アプリが `send_data_stream_closed` / `send_fetch_data_stream_closed` を呼んでも `Ok(())` になり、未知の stream id への呼び出しは従来どおり `SESSION_PROTOCOL_VIOLATION` になること。
- FIN による終端では open な subgroup stream および outgoing FETCH data stream の自動 reset が発行されないこと (cancel と FIN の区別)。
- 上記を `tests/test_session/subscription/`、`tests/test_session/fetch/`、`tests/test_session/data_stream.rs` のテストで固定すること。
- `docs/missing-features.md` §4.7 を解消済みとして更新し、同節を参照する箇所 (要約リストや「要確認」) も更新すること (削除する場合は後続の節番号と参照も更新する)。関連する doc コメント (`fetch_stop_sending_received` / `send_fetch_data_stream_closed` / `Fetch::data_stream_finished` / `SessionEvent::ResetDataStream` など) も新挙動に合わせて更新すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

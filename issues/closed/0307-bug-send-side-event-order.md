# 最終応答の SendOnStream を RequestTerminated より先に発行する

- Created: 2026-10-19
- Completed: 2026-11-10
- Branch: develop
- Polished: 2026-11-10

## 目的

peer FIN 受信済みの request に最終メッセージ (`fin: true` の `SendOnStream`) を送る場合、`Session` はその API 呼び出しで当該 request に積むイベント (最終メッセージの `SendOnStream` と、それに伴う `ResetDataStream`) をすべて積んだ後に、`RequestTerminated { reason: PeerStreamFin }` を発行しなければならない。
逆順だと、アプリは request の終端を観測してから最終メッセージの送信や data stream の reset を指示される。
`RequestTerminated` を受けて request stream の送信半を捨てる、または `forget_track_status` などで状態を破棄する I/O 層では、続く `SendOnStream` が宛先を失い、TRACK_STATUS_OK と FIN を落とす。
これは draft-ietf-moq-transport-23 §9.12 (TRACK_STATUS) の "The bidi stream is closed with a FIN after TRACK_STATUS_OK or TRACK_STATUS_ERROR are sent." に反する。
現状は経路によって順序が逆転している。なお、本リポジトリの `crates/tokio-moq/src/moqt_client.rs` の `drain_events` は `RequestTerminated` で送信半を捨てないため、example では顕在化していない。

## 現状

- `src/session/core.rs` の `send_request_ok` は `mark_send_direction_closed_with_fin` を `SendOnStream` の push より先に呼ぶ。peer FIN 受信済みの TRACK_STATUS では `RequestTerminated` が `SendOnStream` より先に出る。
  - TRACK_STATUS は requester が REQUEST_UPDATE を送れないため (§9.12)、requester は送信直後に FIN してよく (§6.4.2.2 (Graceful Request Stream Closure))、応答前に peer FIN を受けるのが通常の経路になる。
- `mark_send_direction_closed_with_fin` の他の呼び出し元は、`SendOnStream` を先に push してから呼ぶ。
  対象は `src/session/subscription/send.rs` の `send_publish_done`、`src/session/core.rs` の `emit_request_error_with_details` と `send_request_error` の 2 箇所、
  `src/session/data.rs` の `maybe_flush_pending_publish_done`、`src/session/subscription/dispatch.rs` の `handle_err_for_subscription` である。
- ただし `send_request_error` の FETCH 分岐 (Established の FETCH への REQUEST_UPDATE 失敗) は、`SendOnStream(REQUEST_ERROR, fin)` → `mark_send_direction_closed_with_fin` (peer FIN 受信済みなら `RequestTerminated`) → `ResetDataStream` の順で積む。
  終端の観測後に data stream の reset を指示される点で、上と同じ型の逆順である (open issue 0308 も「`ResetDataStream` は `RequestTerminated` より前に発行されること」を求める)。
- `tests/test_session.rs` の `take_send_on_stream_with_fin` は `RequestTerminated` を読み飛ばすため、`tests/test_session/track_status.rs` の `responder_sends_track_status_ok_after_requester_fin` / `requester_fin_then_responder_sends_track_status_ok` は順序を検証していない。
  `tests/test_session/request_stream.rs` の REQUEST_ERROR / PUBLISH_DONE のテスト (例: `fetch_responder_terminates_on_request_error_after_requester_fin`) は helper の直後に `RequestTerminated` を poll するため、逆順なら失敗し、暗黙に順序を固定している。
- `SessionEvent::RequestTerminated` の doc の `PeerStreamFin` の段落は「両方が揃った時点で発行される」とするだけで、同じ API 呼び出しで積む他のイベントとの前後を書いていない。`local_fin_sent` / `mark_send_direction_closed_with_fin` の doc は最終メッセージを「PUBLISH_DONE / 単独 REQUEST_ERROR」としか書かず、TRACK_STATUS_OK が抜けている。

## 設計方針

- 契約を「peer FIN 受信済みの request で `RequestTerminated { reason: PeerStreamFin }` を確定させる API 呼び出しでは、`RequestTerminated` を当該 request に積む最後のイベントにする」と定める。
  - 対象は自側の最終メッセージで終端が確定する経路だけである。peer FIN が後から届く場合は `recv_request_stream_closed` から `finish_request_on_fin_exchange` が別の呼び出しの中で発行するため、順序は自明に成り立つ。Reset / GoawayTimeout / LocalCancel / MalformedTrack は最終メッセージを伴わない。
- `send_request_ok` を「`SendOnStream` push → `mark_send_direction_closed_with_fin`」の順にする。
- `send_request_error` の最初の `if fin { self.mark_send_direction_closed_with_fin(request_id); }` を、FETCH の `ResetDataStream` の push の後 (関数の末尾) へ移す。subscription の PUBLISH_DONE が続く場合は `fin` が偽のため影響しない。
- `SessionEvent::RequestTerminated` の doc の `PeerStreamFin` の段落に上の契約を書く。`local_fin_sent` と `mark_send_direction_closed_with_fin` の doc に、最終メッセージとして TRACK_STATUS_OK を加え、「当該 request のイベントをすべて push した後に呼ぶこと」を書く。
- `take_send_on_stream_with_fin` は変えない (61 箇所が使う helper であり、順序の検証は個別のテストで行う)。

## 完了条件

- `tests/test_session/track_status.rs` の `responder_sends_track_status_ok_after_requester_fin` (と `requester_fin_then_responder_sends_track_status_ok`) で、
  responder 側の `poll_event` が `SendOnStream { fin: true }` の後に `RequestTerminated { reason: PeerStreamFin }` を返すことを、helper を使わずにイベント列で検証すること。修正前の実装では失敗すること。
- Established の FETCH への REQUEST_UPDATE 失敗を peer FIN 受信済みで送るとき、`SendOnStream(REQUEST_ERROR, fin: true)` → `ResetDataStream` → `RequestTerminated { reason: PeerStreamFin }` の順でイベントが出ることをテストで固定すること。修正前の実装では失敗すること。
- `SessionEvent::RequestTerminated`、`local_fin_sent`、`mark_send_direction_closed_with_fin` の doc に設計方針の契約と前提が書かれていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

`src/session/core.rs` の `send_request_ok` で、`SendOnStream` を push してから `mark_send_direction_closed_with_fin` を呼ぶようにした。
`send_request_error` の REQUEST_ERROR 単独 (FIN) の場合の `mark_send_direction_closed_with_fin` を、FETCH の REQUEST_UPDATE 失敗による自動 `ResetDataStream` の push の後 (関数の末尾) へ移した。
これで peer FIN 受信済みの request では、`RequestTerminated { reason: PeerStreamFin }` がその API 呼び出しで当該 request に積む最後のイベントになる。
subscription の PUBLISH_DONE が続く経路は `fin` が偽のため変わらない。

`SessionEvent::RequestTerminated` の doc の `PeerStreamFin` の段落に上の契約を書き、`local_fin_sent` / `mark_send_direction_closed_with_fin` の doc に最終メッセージとして TRACK_STATUS_OK を加え、当該 request のイベントをすべて push した後に呼ぶ前提を書いた。
`skills/shiguredo-moqt/SKILL.md` の request stream の終端の説明にも同じ契約と TRACK_STATUS_OK を加えた。

テストは次のとおり。

- `tests/test_session.rs` にイベント列を取り出す `drain_events` と位置を求める `event_position` を追加した (`tests/test_session/subscription/request_update.rs` の同等のローカル定義は削除した)。
- `tests/test_session/track_status.rs` の `responder_sends_track_status_ok_after_requester_fin` / `requester_fin_then_responder_sends_track_status_ok` で、FIN 付きの TRACK_STATUS_OK が `RequestTerminated(PeerStreamFin)` より先に出て、`RequestTerminated` が最後のイベントであることを検証する。
- `tests/test_session/fetch/request_update.rs` に `request_error_update_failure_after_requester_fin_terminates_last` を追加し、`SendOnStream(REQUEST_ERROR, fin)` → `ResetDataStream` → `RequestTerminated(PeerStreamFin)` の順と、`RequestTerminated` が最後であることを固定した。
- 3 件とも、修正前の `core.rs` では失敗し、修正後に通ることを確認した。

`CODEBASE.md` の規定に従い、ブランチと PR を作らず develop に直接コミットし、`CHANGES.md` は更新していない。

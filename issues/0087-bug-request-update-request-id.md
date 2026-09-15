# REQUEST_UPDATE に新しい Request ID を採番する

- Created: 2026-09-15
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-request-update-request-id
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §6.4.2.1 (Request ID) は REQUEST_UPDATE を Request ID を消費する
メッセージとして列挙し、重複した Request ID の受信を `INVALID_REQUEST_ID` での
セッションクローズ MUST としている。また §3.4 (Joining a Subscription) は fill fetch stream が
載せる Request ID を「初回は SUBSCRIBE の Request ID、以降は REQUEST_UPDATE の Request ID」と定め、
REQUEST_UPDATE によって同時に複数開いた fill fetch stream を Request ID で識別できることを要求している。
現状は REQUEST_UPDATE が購読の Request ID を再利用するため重複 Request ID の MUST 違反になり、
複数の fill fetch stream を識別できず、draft 準拠の peer との間で双方向にセッションが閉じる。

## 現状

- `src/session/subscription/send.rs` の `send_request_update` は `RequestIds::local_generator` から
  新しい Request ID を採番せず、引数の購読の Request ID を `RequestUpdate` と `SendOnStream` の
  両方にそのまま使う。`next_id` を呼ぶのは `send_subscribe` と `send_publish` だけである。
- `src/session/subscription/recv.rs` の `handle_peer_request_update` は wire の `request_id` と
  bidi request stream の context の `request_id` の一致を必須にし、不一致を
  `SESSION_PROTOCOL_VIOLATION` で `fail` する。draft 準拠の peer が REQUEST_UPDATE に
  新しい Request ID を載せると、この一致検査でセッションが閉じる。
- fill fetch stream は購読の Request ID でしか識別されない。publisher 側は
  `src/session/subscription/fill.rs` の `maybe_open_fill_stream` が
  `SessionEvent::OpenFillFetchStream` に購読の Request ID を載せ、subscriber 側は
  `src/session/data.rs` の `recv_fetch_header` が `subscriptions` を `header.request_id` で引き、
  未知なら `SESSION_PROTOCOL_VIOLATION` で `fail` する。REQUEST_UPDATE 起因の fill と初回 fill が
  同一の Request ID を持つため、§3.4 の「同時に複数開いた fill fetch stream を Request ID で
  識別する」を満たせない。
- `ControlMessage::RequestUpdate` を構築する production の経路は `send_request_update` の 1 箇所だけで、
  REQUEST_UPDATE の Request ID から購読の Request ID への対応表は存在しない。
  MAX_REQUEST_UPDATES の残数管理 (`outgoing_request_updates` / `incoming_request_updates`) は
  購読の Request ID をキーにしており、こちらは stream context 単位で正しい。

根拠 (draft-ietf-moq-transport-21 §6.4.2.1 (Request ID)):

> Request ID is included in request messages and is used to identify requests across messages.
> For example, fetch streams reference the Request ID of a SUBSCRIBE, PUBLISH, FETCH, or REQUEST_UPDATE.

> Each SUBSCRIBE, PUBLISH, FETCH, SUBSCRIBE_NAMESPACE, SUBSCRIBE_TRACKS, PUBLISH_NAMESPACE,
> REQUEST_UPDATE, and TRACK_STATUS message consumes a Request ID.

> If an endpoint receives a Request ID where the least significant bit is incorrect for the sender,
> or a duplicate Request ID, it MUST close the session with INVALID_REQUEST_ID.

根拠 (draft-ietf-moq-transport-21 §3.4 (Joining a Subscription)):

> The FETCH_HEADER on the fill fetch stream carries the Request ID of the message that initiated it:
> the SUBSCRIBE Request ID for the initial fill, or the REQUEST_UPDATE Request ID for a subsequent fill.
> As a result of REQUEST_UPDATE, a subscription can have multiple fill fetch streams open at once,
> each identified by its Request ID;

再現手順 (送信側):

1. `send_subscribe` で購読を確立する
2. 同じ bidi request stream で `send_request_update` を呼ぶ
3. wire の `RequestUpdate` の Request ID が SUBSCRIBE と同一になり、draft 準拠の peer は
   重複 Request ID として `INVALID_REQUEST_ID` でセッションを閉じる

再現手順 (受信側):

1. peer が `REQUEST_UPDATE` に購読とは異なる新しい Request ID を載せて送る
2. `handle_peer_request_update` の一致検査が不一致と判定し、`SESSION_PROTOCOL_VIOLATION` で
   セッションを閉じる

## 設計方針

- `send_request_update` で `next_id` により新しい Request ID を採番し、wire の `RequestUpdate` に
  採番した ID を載せる。`SendOnStream` の `request_id` は送信先 bidi request stream の識別子
  (購読の Request ID) のままとし、購読の状態を引くキーも従来どおり購読の Request ID とする。
  採番は既存の `send_subscribe` / `send_publish` と同じく全検証の後に行い、送信されなかった
  REQUEST_UPDATE が peer 側の Request ID の欠番を作らないようにする。
- 採番した Request ID から購読の Request ID への対応を `Session` に保持する。登録するのは
  FILL_PARAMETERS を持つ REQUEST_UPDATE だけとする (fill fetch stream を開きうるのはその場合だけ)。
- `handle_peer_request_update` から wire の Request ID と stream context の一致必須を外す。
  購読の解決は bidi request stream の context で行い、wire の Request ID を
  「この REQUEST_UPDATE の Request ID」として対応に記録する。
- `maybe_open_fill_stream` は REQUEST_UPDATE 起因の呼び出しでは REQUEST_UPDATE の Request ID を
  `SessionEvent::OpenFillFetchStream` に載せる。解決は共通の `resolve_fill_subscription` が行い、
  REQUEST_UPDATE の Request ID の対応を先に引いてから購読の Request ID として解決する。この順序に
  より、REQUEST_UPDATE の Request ID が別 subscription の Request ID と偶然一致しても起因メッセージの
  購読へ帰属させる。`recv_fetch_header` と `send_fill_fetch_header` はこの解決を通し、どちらにも
  無い Request ID は従来どおり `PROTOCOL_VIOLATION` とする。
- MAX_REQUEST_UPDATES の残数管理と REQUEST_OK の対応付けは bidi request stream 単位の現行構造を
  変えない。
- 購読の終端時に、その購読に属する REQUEST_UPDATE の Request ID の対応を破棄する。
- §6.4.2.1 が受信側に課す Request ID の parity 不正・重複の検出 (REQUEST_UPDATE の Request ID を
  peer の Request ID 空間で検証すること) は本 issue の範囲外とする。本 issue は送信側の採番と、
  購読とは異なる Request ID を持つ REQUEST_UPDATE の受理・fill fetch stream の識別を対象にする。

## 完了条件

- `send_request_update` が載せる wire の Request ID が購読の Request ID と重複せず、parity も
  送信側の規則に一致すること
- draft 準拠の peer が REQUEST_UPDATE に新しい Request ID を載せても
  `handle_peer_request_update` がセッションを閉じないこと
- REQUEST_UPDATE 起因の fill fetch stream が REQUEST_UPDATE の Request ID で識別され、
  初回 fill と同時に開けること
- 送信側と受信側それぞれの回帰テストが `tests/test_session/` に追加され、
  `cargo test --workspace` が通ること
- `cargo clippy --workspace --all-targets -- -D warnings` と `cargo fmt --all -- --check` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること

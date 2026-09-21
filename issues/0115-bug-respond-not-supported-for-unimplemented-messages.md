# 未実装の定義済みメッセージに NOT_SUPPORTED を返す

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-respond-not-supported-for-unimplemented-messages
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §1.5 (Modularity) は、限定的な endpoint に未対応メッセージへ NOT_SUPPORTED で応答することを求めている。

> Limited endpoints SHOULD respond to any unsupported messages with the
> appropriate NOT_SUPPORTED error code, rather than ignoring them.

§6.3 (Session initialization) は request stream の先頭として 7 種を合法と定める。

> A request stream begins with one of these seven message types:
> TRACK_STATUS, SUBSCRIBE, PUBLISH, FETCH, PUBLISH_NAMESPACE,
> SUBSCRIBE_NAMESPACE, and SUBSCRIBE_TRACKS.  Bidirectional streams
> MUST NOT begin with any other message type unless negotiated.

現状は、定義済みだが未実装の PUBLISH_NAMESPACE (0x06) / SUBSCRIBE_NAMESPACE (0x50) / SUBSCRIBE_TRACKS (0x51) を decode 段階で拒否するため、合法な request 1 通でセッション全体が落ちる。§9 (Control Messages) がセッション終了を MUST とするのは本当に未知のメッセージ型であり、定義済みの型を未知として扱うのは過剰である。relay 専用機能を実装しない方針は維持したまま、未対応であることを peer へ正しく伝える。

## 現状

- `src/message.rs` の `ControlMessage` に 0x06 / 0x50 / 0x51 / 0x08 / 0x0E / 0x0F の variant が無く、`ControlMessage::decode_message_body` が `MessageError::InvalidMessageType` を返す。
- `MessageError::InvalidMessageType` の doc のとおり、この拒否は I/O 層がセッション終了として扱う。`Session::recv_request` は decode 済みの `ControlMessage` を受け取るため、回復できない。
- `src/error.rs` の `REQUEST_NOT_SUPPORTED` (0x3) は定義済みだがライブラリ内で使用されていない。examples では publisher の `serve_peer_request` と subscriber のイベントループが未対応 request の拒否に使うのみである。
- `docs/IMPLEMENTATION.md` の「未対応」と `CHANGES.md` の `## develop` に、これら 6 種を実装せず受信時は `SESSION_PROTOCOL_VIOLATION` でセッションを閉じると記載している。
- §9 の Table 5 では 0x06 / 0x50 / 0x51 が Request, First、0x08 (NAMESPACE) / 0x0E (NAMESPACE_DONE) / 0x0F (PUBLISH_SKIPPED) が Request (First なし) である。後者 3 種は SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS への応答であり、自側が要求していなければ到着し得ない。

## 設計方針

- 未対応メッセージを型として識別できる状態にする。`ControlMessage` に variant を追加するか、未対応を表す variant (型 ID と payload) を追加するかは実装側で決める。いずれの場合も `ControlMessage::decode_message_body` が `InvalidMessageType` を返さないようにする。
- request stream の先頭で受けた PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS は、`Session::recv_request` で
  `REQUEST_ERROR` / `NOT_SUPPORTED` を返して送信方向を FIN し、セッションは維持する (draft §1.5 / §6.4.2.3)。FIN は
  `Session::emit_request_error` と同じく `SessionEvent::SendOnStream { fin: true }` で表現する。request は登録せず、
  `Session::recv_request_stream_closed` が拒否済み ID として no-op で吸収する既存経路に載せる。
- 応答専用の NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED は、自側が SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS を送らないため到着し得ない。型として識別したうえで、受信は従来どおり `PROTOCOL_VIOLATION` とする (応答先の request が無く、REQUEST_ERROR を返す対象にならない)。この判断を doc コメントとテストで固定する。
- relay 専用機能を実装しない方針 (CODEBASE.md) は変えない。名前空間の告知・発見の状態機械は追加しない。
- TRACK_STATUS の受信対応 (0109) と関連する。TRACK_STATUS を受信可能にするかは 0109 の判断に従い、本 issue は上記 3 種の未対応 request と 3 種の応答専用メッセージの扱いに限定する。
- `docs/IMPLEMENTATION.md` と `CHANGES.md` の記述を実装に合わせて更新する。

## 完了条件

- PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS を request stream の先頭として受信してもセッションが `Established` のまま維持され、`REQUEST_ERROR` の `NOT_SUPPORTED` (0x3) と送信方向の FIN が発行されることがテストで固定されていること。
- NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED を受信した場合の扱い (型として識別でき、セッションは `PROTOCOL_VIOLATION` で閉じる) がテストで固定されていること。
- §9 の Table 5 に定義が無いメッセージ型が引き続き `InvalidMessageType` で拒否されることがテストで固定されていること。
- `docs/IMPLEMENTATION.md` と `CHANGES.md` の記述が実装と一致していること。
- `cargo test --workspace` が通ること。

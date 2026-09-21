# 未実装の定義済み request メッセージに NOT_SUPPORTED を返す

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-respond-not-supported-for-unimplemented-messages
- Polished: 2026-09-21

## 目的

draft-ietf-moq-transport-21 §1.5 (Modularity) は、限定的な endpoint に未対応メッセージへ NOT_SUPPORTED で応答することを求めている。

> Limited endpoints SHOULD respond to any unsupported messages with the
> appropriate NOT_SUPPORTED error code, rather than ignoring them.

§6.3 (Session initialization) は request stream の先頭として 7 種を挙げる。

> A request stream begins with one of these seven message types:
> TRACK_STATUS, SUBSCRIBE, PUBLISH, FETCH, PUBLISH_NAMESPACE,
> SUBSCRIBE_NAMESPACE, and SUBSCRIBE_TRACKS.  Bidirectional streams
> MUST NOT begin with any other message type unless negotiated.

現状は、定義済みだが未実装の PUBLISH_NAMESPACE (0x06) / SUBSCRIBE_NAMESPACE (0x50) / SUBSCRIBE_TRACKS (0x51) を decode 段階で拒否するため、合法な request 1 通でセッション全体が落ちる。§9 (Control Messages) がセッション終了を MUST とするのは本当に未知のメッセージ型であり、定義済みの型を未知として扱うのは過剰である。

§6.3 と Table 5 が request stream の先頭として合法と定めている以上、peer がこれらを送る場合にセッションを落とすのは仕様に反する。relay 専用機能を実装しない方針は維持したまま、未対応であることを peer へ正しく伝える。

本 issue が NOT_SUPPORTED を返すのは request として届く 3 種 (0x06 / 0x50 / 0x51) である。応答専用の 3 種 (0x08 / 0x0E / 0x0F) は §9.16 / §9.17 / §9.19 のとおり自側が SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS を送らない限り到着せず、応答先の request を持たないため `PROTOCOL_VIOLATION` のままとする。

## 現状

- `src/message.rs` の `ControlMessage` に 0x06 / 0x50 / 0x51 / 0x08 / 0x0E / 0x0F の variant が無く、未知 type_id の弾きは `ControlMessage` の private な `decode_message_body(type_id, payload)` の `_ =>` にある。公開の `ControlMessage::decode` はこれを呼ぶだけである。各メッセージ構造体の `pub(crate)` な `decode_message_body(payload)` は型 ID の拒否を行わない
- `MessageError::InvalidMessageType` の doc のとおり、この拒否は I/O 層がセッション終了として扱う。`Session::recv_request` は decode 済みの `ControlMessage` を受け取るため、回復できない
- `ControlMessage::decode` は型ごとのデコード後に「ペイロード長と実際の消費バイト数が一致するか」を検証しており、未対応型を variant 化する場合はこの検証と衝突しない形にする必要がある
- `Session::recv_request` の `_ =>` は `SESSION_PROTOCOL_VIOLATION` でセッションを閉じ、`Session::recv_stream_message` (request stream の 2 通目以降) にも別の `_ =>` がある
- `src/error.rs` の `REQUEST_NOT_SUPPORTED` (0x3) は定義済みだがライブラリ内で使用されていない。examples では publisher の `serve_peer_request` と subscriber のイベントループが未対応 request の拒否に使うのみである
- `docs/IMPLEMENTATION.md` の「未対応」と `CHANGES.md` の `## develop` に、これら 6 種を実装せず受信時は `SESSION_PROTOCOL_VIOLATION` でセッションを閉じると記載している
- §9 の Table 5 では 0x06 / 0x50 / 0x51 が Request, First、0x08 (NAMESPACE) / 0x0E (NAMESPACE_DONE) / 0x0F (PUBLISH_SKIPPED) が Request (First なし) である。後者 3 種は SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS への応答であり、自側が要求していなければ到着し得ない
- 0x06 / 0x50 / 0x51 のメッセージ本体は §9.14 / §9.15 / §9.18 のとおり Request ID (vi64) で始まる。0x0E / 0x0F の本体は `Track Namespace Suffix` で始まり Request ID を持たない

## 設計方針

- `ControlMessage` に `Unsupported { type_id: u64, request_id: Option<u64>, body: Vec<u8> }` を追加する。定義済みの 6 種 (0x06 / 0x50 / 0x51 / 0x08 / 0x0E / 0x0F) だけをこの variant として返す
- Table 5 に無い型は従来どおり `ControlMessage::decode` が `MessageError::InvalidMessageType` を返す。定義済み未対応型と未知型の切り分けは型 ID の allowlist で行う
- `request_id` は本体が Request ID で始まる 0x06 / 0x50 / 0x51 だけ `Some` にする。0x08 / 0x0E / 0x0F は `None` にする
- 0x06 / 0x50 / 0x51 で本体が空、または Request ID の vi64 が途中で切れている場合は `ControlMessage::decode` の `Err` とする。既存の decode エラーと同じく I/O 層がセッション終了として扱う
- `body` は Length の後ろの生バイト列を保持し、`ControlMessage::encode` が type + Length + body を再構成できるようにする。`ControlMessage::decode` の末尾長一致検証 (`consumed != payload.len()`) と衝突しないよう、この分岐では payload 全体を消費したものとして扱う
- `Session::recv_request` (request stream の先頭) に `ControlMessage::Unsupported` の分岐を追加する
  - `type_id` が 0x06 / 0x50 / 0x51 なら、まず `Session::validate_peer_request_id` で parity と重複を検証する (違反は §6.4.2.1 の MUST により `INVALID_REQUEST_ID` でセッションを閉じる)
  - 検証を通ったら `Session::emit_request_error(request_id, REQUEST_NOT_SUPPORTED, ...)` を発行して `Ok(())` を返す。request は `request_streams` に登録しないため、後続の終端通知は `rejected_request_ids` の no-op 経路で吸収される
  - `type_id` が 0x08 / 0x0E / 0x0F なら `SESSION_PROTOCOL_VIOLATION` を返して `Session::fail` する。応答先の request を持たないため REQUEST_ERROR を返す対象にならない
- `Session::recv_stream_message` (request stream の 2 通目以降) にも同じ分岐を追加する。0x08 / 0x0E / 0x0F はここでも `PROTOCOL_VIOLATION` にする。Table 5 で First を持つ 0x06 / 0x50 / 0x51 が 2 通目以降に届く場合は同表の注記 ("Messages marked \"First\" MUST be the first message on a new request stream.") に反するため、`PROTOCOL_VIOLATION` のままとする
- 拒否コードの優先順を固定する。自側が control GOAWAY を送信済みなら `GOING_AWAY` を優先し、そうでなければ `NOT_SUPPORTED` を返す。parity と重複の検証はどちらより先に行う
- 未対応 request は本体を解析しないため `Session::accept_peer_request` を通せない (同関数は認証トークン REGISTER の適用に `&MessageParameters` を要求する)。したがって GOING_AWAY の判定は `self.goaway.local_sent` を直接見て行い、0114 の「publisher として応答する request に限る」という種別判定は適用しない (未対応 request は publisher 役で受ける request であり、種別を解析できない)
- 0109 (TRACK_STATUS の受信実装) との関係: `Session::recv_request` の `ControlMessage::TrackStatus` 分岐とは独立であり、`Unsupported` の分岐は `_ =>` の直前に置く
- relay 専用機能を実装しない方針 (CODEBASE.md) は変えない。名前空間の告知・発見の状態機械は追加しない
- `ControlMessage` は `#[non_exhaustive]` でない公開 enum であり variant 追加は破壊的変更である。`CHANGES.md` の `## develop` に `[CHANGE]` を追加する
- `docs/IMPLEMENTATION.md` と `CHANGES.md` の記述を実装に合わせて更新する

## 完了条件

- PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS を request stream の先頭として受信してもセッションが `Established` のまま維持され、`REQUEST_ERROR` の `NOT_SUPPORTED` (0x3) と送信方向の FIN が発行されることがテストで固定されていること
- 拒否した request の後続の終端通知が `rejected_request_ids` の no-op 経路で吸収されることがテストで固定されていること
- 未対応 request の parity 違反と重複が `INVALID_REQUEST_ID` でセッションを閉じることがテストで固定されていること
- 自側が control GOAWAY を送信済みの場合は `GOING_AWAY` を返すことがテストで固定されていること
- NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED を request stream の先頭で受けた場合と 2 通目以降で受けた場合の両方で、セッションが `PROTOCOL_VIOLATION` で閉じることがテストで固定されていること
- §9 の Table 5 に定義が無いメッセージ型が引き続き `MessageError::InvalidMessageType` で拒否されることがテストで固定されていること
- `ControlMessage::Unsupported` の `encode` が decode 前のバイト列を再構成できることがテストで固定されていること
- `docs/IMPLEMENTATION.md` と `CHANGES.md` の記述が実装と一致し、`CHANGES.md` の `## develop` に `[CHANGE]` が追加されていること
- `cargo test --workspace` が通ること

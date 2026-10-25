# Malformed Track 検出時に同じ Track の全 subscription / FETCH を cancel する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) は "When a subscriber detects a Malformed Track, it MUST cancel any
corresponding subscription or fetches for that Track from that publisher" と MUST を課す。draft-22 は同一 Track への複数同時 subscription
を許可しており、1 本の subscription で malformed を検出したとき、同じ Track に対する他の subscription / FETCH を残すとこの MUST を満たせない。現状は検出した request だけを終端している。

## 現状

- `src/session/data.rs` の `Session::terminate_malformed_track` は引数の `request_id` に対応する 1 件だけを `Terminated`
  にし、`SessionEvent::StopSendingRequestStream` → `ResetRequestStream` → `RequestTerminated { reason: MalformedTrack }` を発行する。doc
  コメントにも「他の request は生き残る」と書かれている。
- `src/session/subscription.rs` の `Session::register_subscription` は `AliasState::subscriptions_by_track` を (Track Namespace,
  Track Name, TrackRole) キーで維持しており、同一 Track の subscription を列挙できる。しかし malformed 終端ではこの索引を使っていない。
- `src/session/fetch.rs` の `Fetch` は publisher 役 / subscriber 役とも `track_namespace` / `track_name` を保持するが、malformed 終端は fetch
  と連動しない。
- 同一 Track への複数同時 subscription と Track Alias 共有は `tests/test_session/subscription/handshake.rs` と
  `tests/test_session/shared_track_alias.rs` で許可済みであり、同じ Track の request が同時に複数存在しうる。

## 設計方針

- malformed を検出した request の Track Namespace / Track Name を使い、同じ Track に対する自側 subscriber 役の subscription と FETCH をすべて列挙し、それぞれを
  malformed 終端する。
- 列挙には `AliasState::subscriptions_by_track` と `Session.fetches` を使う。publisher 役の request は malformed 検出の対象ではないため対象外とする。
- 各 request について、bidi request stream の受信方向 STOP_SENDING と送信方向 RESET_STREAM、`RequestTerminated { reason: MalformedTrack }` を発行する。
- 各 request に属する open な受信 data stream がある場合は STOP_SENDING を発行する。検出済み stream の reset と終端通知の重複発行を抑止する既存の扱いを維持する。
- 対象 request ID を先に収集してから 1 件ずつ終端し、借用の衝突と二重終端を避ける。

## 完了条件

- 同一 Track への 2 本の subscription があり、1 本の data stream で malformed を検出したとき、両方の subscription が `Terminated` になり、それぞれに STOP_SENDING /
  RESET_STREAM / `RequestTerminated` が 1 回ずつ発行されること。
- 同一 Track への subscription と FETCH が併存するとき、FETCH も終端されること。別 Track の request と publisher 役の request は影響を受けないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

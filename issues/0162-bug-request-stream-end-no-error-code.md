# RequestStreamEnd::Reset に「アプリケーションエラーコード無し」を表す値が無い

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-request-stream-end-no-error-code
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) は、WT_APPLICATION_ERROR の範囲外のエラーコードで RESET_STREAM / STOP_SENDING を受信した場合に
「アプリケーションエラーコード無し」のストリームリセットとして application へ届ける SHOULD を定める。

> If a RESET_STREAM or STOP_SENDING frame is received with an error code outside the range reserved for WT_APPLICATION_ERROR, the stream is still considered reset, but the error code is not mapped to a WebTransport application error code.
> The WebTransport implementation SHOULD deliver this to the application as a stream reset with no application error code.

`RequestStreamEnd` は `error_code: u64` を必須で持ち「無し」を表せないため、WebTransport 経路では wire の HTTP/3 コードをそのまま入れており、
peer が素の MOQT のコード (0x0-0x12) を載せた場合に MOQT のコードと区別できない。

## 現状

- `src/session/types.rs` の `RequestStreamEnd::Reset` は `error_code: u64` と `reliable_size: Option<u64>` を持つ (`src/session/types.rs` の定義)。
- `terminationreason_from_end` は `RequestStreamEnd::Reset { error_code, .. }` を `TerminationReason::PeerStreamReset { error_code }` にそのまま渡す。
- `examples/moqt-transport/src/webtransport.rs` の `wt_reset_error_code` は remap できない値で wire の HTTP/3 コードをそのまま返し、
  `tracing::warn!` に生値を残すだけである (QUIC 経路は QUIC のコード空間のため常に remap 済みの値になる)。
- `RequestStreamEnd::Reset` の出現はリポジトリ全体で 67 箇所あり、構築サイトは `src/session/`、`tests/`、`pbt/tests/`、`examples/` に分布する。
  型を `Option<u64>` にすると `TerminationReason::PeerStreamReset` とその利用側 (ログ・テスト・セッション状態) にも波及する。

## 設計方針

- `RequestStreamEnd::Reset` の `error_code` を `Option<u64>` にし、`None` を「アプリケーションエラーコード無し」とする
- `TerminationReason::PeerStreamReset` の `error_code` も同じ表現にそろえる (`None` のときログはコード無しとして出す)
- QUIC 経路 (`moqt://`) は QUIC のコード空間で常にコードがあるため `Some` を入れる
- WebTransport 経路は remap できたとき `Some(remap 後の MOQT のコード)`、できないとき `None` を入れ、生値は `warn` ログにのみ残す
- 型変更に伴う構築サイト・パターンマッチの追従は機械的に行い、`None` の経路だけをテストで固定する

## 完了条件

- `RequestStreamEnd::Reset { error_code: None }` が「アプリケーションエラーコード無し」として扱われること
- WebTransport 経路で範囲外 / 予約コードポイントを受信したとき `error_code` が `None` になり、生値は warn ログに残ること
- remap できたときは `Some(MOQT のコード)` が入ること、QUIC 経路は `Some(QUIC のコード)` のままであること
- `TerminationReason::PeerStreamReset` に `None` が伝わり、既存の `Some` の挙動が変わらないこと
- 既存の `src/session/`、`tests/`、`pbt/`、`examples/` のテストが `None` 対応後も通ること

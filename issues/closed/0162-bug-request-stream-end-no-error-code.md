# RequestStreamEnd::Reset に「アプリケーションエラーコード無し」を表す値が無い

- Created: 2026-09-24
- Completed: 2026-09-30
- Branch: feature/fix-request-stream-end-no-error-code
- Polished: 2026-09-27

## 目的

draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) は、WT_APPLICATION_ERROR の範囲外のエラーコードで RESET_STREAM / STOP_SENDING を受信した場合に
「アプリケーションエラーコード無し」のストリームリセットとして application へ届ける SHOULD を定める。

> If a RESET_STREAM or STOP_SENDING frame is received with an error code outside the range reserved for WT_APPLICATION_ERROR, the stream is still considered reset, but the error code is not mapped to a WebTransport application error code.
> The WebTransport implementation SHOULD deliver this to the application as a stream reset with no application error code.

`RequestStreamEnd` は `error_code: u64` を必須で持ち「無し」を表せないため、WebTransport 経路では remap できないときに wire の HTTP/3 コードをそのまま入れており、
peer が素の MOQT のコード (0x0-0x12) を載せた場合に MOQT のコードと区別できない。

## 現状

- `src/session/types.rs` の `RequestStreamEnd::Reset` は `error_code: u64` と `reliable_size: Option<u64>` を持つ (`src/session/types.rs` の定義)。
- `terminationreason_from_end` は `RequestStreamEnd::Reset { error_code, .. }` を `TerminationReason::PeerStreamReset { error_code }` にそのまま渡す。
- `examples/moqt-transport/src/webtransport.rs` の `wt_reset_error_code` は remap できない値で wire の HTTP/3 コードをそのまま返し、
  `tracing::warn!` に生値を残すだけである (QUIC 経路は QUIC のコード空間のため remap 不要で、入る値は常に MOQT のコードである)。
- `RequestStreamEnd::Reset` の出現はリポジトリ全体 (`*.rs`) で 65 箇所あり、構築サイトは `src/session/`、`tests/`、`pbt/tests/`、`examples/`、`fuzz/` に分布する。
  型を `Option<u64>` にすると `TerminationReason::PeerStreamReset` とその利用側 (ログ・テスト・セッション状態) にも波及する。

## 設計方針

- `RequestStreamEnd::Reset` の `error_code` を `Option<u64>` にし、`None` を「アプリケーションエラーコード無し」とする
- peer から受信した STOP_SENDING 側 (送信ストリームのエラーとして届く経路) は [issues/0163](../issues/0163-bug-received-stop-sending-code-remap.md) が扱い、
  本 issue は RESET_STREAM 受信側の `RequestStreamEnd` だけを対象とする
- `TerminationReason::PeerStreamReset` の `error_code` も同じ表現にそろえる (`None` のときログはコード無しとして出す)
- QUIC 経路 (`moqt://`) は QUIC のコード空間で常にコードがあるため `Some` を入れる
- WebTransport 経路は remap できたとき `Some(remap 後の MOQT のコード)`、できないとき `None` を入れ、生値は `warn` ログにのみ残す
- 型変更に伴う構築サイト・パターンマッチの追従は機械的に行い、`None` の経路だけをテストで固定する

## 完了条件

- `RequestStreamEnd::Reset { error_code: None }` が「アプリケーションエラーコード無し」として扱われること
- WebTransport 経路で範囲外 / 予約コードポイントを受信したとき `error_code` が `None` になり、生値は warn ログに残ること
- remap できたときは `Some(MOQT のコード)` が入ること、QUIC 経路は `Some(QUIC のコード)` のままであること
- `TerminationReason::PeerStreamReset` に `None` が伝わり、既存の `Some` の挙動が変わらないこと
- 既存の `src/session/`、`tests/`、`pbt/`、`examples/` のテストと `fuzz/` のターゲットが `None` 対応後も通ること

## 解決方法

`RequestStreamEnd::Reset` の `error_code` と `TerminationReason::PeerStreamReset` の `error_code` を `Option<u64>` にし、`None` を「アプリケーションエラーコード無し」とした。

- `src/session/types.rs`: `RequestStreamEnd::Reset { error_code: Option<u64>, .. }` と `TerminationReason::PeerStreamReset { error_code: Option<u64> }` に変更し、`None` の意味 (draft-ietf-webtrans-http3-16 §4.4 の "no application error code") と QUIC 経路では常に `Some` になることを doc に明記した。
  `terminationreason_from_end` はそのまま値を引き継ぐ
- `examples/tokio-moq/src/webtransport_h3.rs`: `wt_reset_error_code` の戻り値を `Option<u64>` にし、remap できたときは `Some(MOQT のコード)`、できないとき (WT_APPLICATION_ERROR の範囲外と予約コードポイント) は `None` を返すようにした。生値は従来どおり `tracing::warn!` に残る。`u64` のまま `wire` の値を入れていた暫定実装 (型変更までの暫定と doc に書かれていた) を解消した
- `examples/tokio-moq/src/transport.rs` (QUIC 経路): QUIC のコード空間で常にコードがあるため `Some(error.into())` を入れる
- `examples/tokio-moq/src/webtransport_h2.rs`: over HTTP/2 の `WT_RESET_STREAM` は MOQT §12.5 のコードをそのまま運ぶため `Some(error_code)` を入れる (HTTP/3 のような code space の remap は不要)
- `examples/moq-pub/src/stream_writer.rs`: reset 時に `error_code` を `Some(...)` で渡す
- 型変更に伴う構築サイト・パターンマッチの追従は `src/session/`、`tests/`、`pbt/`、`fuzz/` の全域で機械的に行った (38 箇所)
- `skills/shiguredo-moqt/SKILL.md` に `error_code` が `Option<u64>` であることと `None` の意味を追記した

追加・更新したテスト:

- `tests/test_session/request_stream.rs`: `request_stream_reset_without_error_code_is_terminated_with_none` を追加し、`error_code: None` の reset が `TerminationReason::PeerStreamReset { error_code: None }` として通知されることを固定した。既存の `Some(42)` のテストは `Some(Some(42))` として維持した
- `examples/tokio-moq/src/webtransport_h3.rs`: `wt_reset_error_code_returns_moqt_code_or_passes_through` と
  `wt_reset_error_code_warns_reserved_code_points_separately` を `Some` / `None` に更新し、`wt_reset_stream_end_uses_remapped_code` と
  `wt_recv_end_maps_stream_reset_to_remapped_request_stream_end` で remap できないコードが `None` になることを固定した
- `fuzz/fuzz_targets/fuzz_session.rs` と `pbt/tests/prop_session/` は `Option<u64>` に追随させた

未実施の確認:

- 実機確認は WebTransport セッションの確立自体が `issues/pending/0094` の解消待ちであるため行っていない。remap と `None` の伝播は単体テストで固定した

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` / `cargo build --manifest-path fuzz/Cargo.toml` が通ることを確認した。

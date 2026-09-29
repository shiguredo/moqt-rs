# 32 ビットに収まらない close code で WebTransport セッションを閉じられない

- Created: 2026-09-26
- Completed: 2026-09-30
- Branch: feature/fix-moqt-close-code-over-32-bits
- Polished: 2026-09-27

## 目的

`StreamHandle::close` に 32 ビットを超える MOQT の close code が渡された場合でも、接続を閉じる手段を確保する。MOQT §13 (Grease) の greasing 値は `0x7f * N + 0x9D` で 32 ビットを超える値を取り得る。`WT_CLOSE_SESSION` capsule の Application Error Code は 32 ビットのため、これらの値は capsule では運べない。

## 現状

- `examples/moqt-transport/src/webtransport.rs` の `moqt_close_code` は `u32::try_from` で変換し、収まらない場合は `TransportError::Internal` を返す (切り捨てない)。
- `examples/moqt-transport/src/transport.rs` の `StreamHandle::close` はこの変換結果を `WtSession::close` へ渡すため、変換に失敗すると閉じる操作自体が失敗する。`WtSession::close` は `WT_CLOSE_SESSION` を送って CONNECT stream に FIN を送る経路であり、失敗すると接続も WebTransport のセッション状態 (`WtSessionState`) も閉じないまま残る。
- `examples/moqt-transport/src/moqt_client.rs` の `MoqtClient::close` は `drain_events` の `SessionEvent::CloseSession` 分岐で `self.handle.close(err.code, err.reason).await?` とするため、変換失敗を `Err(TransportError::Internal)` として呼び出し元へ返す。
- `examples/moqt-subscriber/src/pipeline.rs` の `close_session` はこのエラーに `warn!` を出して継続するため、セッションは閉じないまま残る。
- 現行の example が `close` に渡すのは、publisher の `0` と subscriber の `0` および `session_error_code` が返す小さなコード (0x3 / 0x6) だけである。32 ビットを超える close code は `StreamHandle::close` / `Session::close` / `MoqtClient::close` が任意の `u64` を受け付ける公開 API 経由でのみ到達し得る。
- `WT_CLOSE_SESSION` capsule の Application Error Code は 32 ビットである (draft-ietf-webtrans-http3-16 §6)。QUIC の
  CONNECTION_CLOSE が運ぶアプリケーションエラーコードは varint であり 32 ビットに限らない (RFC 9000 §19.19)。
  依存 s2n-quic の `application::Error::new` が受け付けるのは varint の上限 2^62-1 までであり、既存の QUIC 経路
  (`StreamHandle::close` の QUIC 分岐) は超える値を `Error::UNKNOWN` に落として接続を閉じている。

## 設計方針

- 32 ビットに収まらない場合は `s2n_quic::connection::Handle::close` で CONNECTION_CLOSE を送る。`WtSession` は
  `handle: s2n_quic::connection::Handle` を保持しており `Handle::close` は `&self` で呼べるため、
  `WtSession::close` の経路から送れる。`WT_CLOSE_SESSION` の詳細メッセージは送れないが、接続は閉じる。
  CONNECTION_CLOSE の application error code には元の MOQT の close code をそのまま載せる。
- varint の上限 (2^62-1) を超える値は `s2n_quic::application::Error::new` が受け付けないため、既存の QUIC 経路と同じく `Error::UNKNOWN` に落として接続を閉じる (コードは伝わらない)。
- フォールバックでもセッション状態を終了へ移し、既存ストリームの `WT_SESSION_GONE` での中断と新規ストリーム / datagram の拒否 (draft-ietf-webtrans-http3-16 §6 の MUST / MUST NOT) を効かせる。
- MOQT §6.6 は WebTransport 経路の終了を `CLOSE_WEBTRANSPORT_SESSION` capsule と定めるが、capsule の Application Error Code は 32 ビットのため収まらないコードは運べない。draft-ietf-webtrans-http3-16 §6 は CONNECT stream の close もセッション終了の条件とするため、接続レベルの close に切り替える。
- 代替案の `SessionError::code` の 32 ビット制限は、公開 API の変更になり QUIC 経路で送れる値も狭めるため採らない。本 issue は接続レベルの close を採用し、コードを切り捨てずに閉じる操作を成立させる。
- reset / STOP_SENDING 経路 (draft-ietf-webtrans-http3-16 §4.4) の 32 ビット超のコードの扱いは [issues/0164](../issues/0164-change-webtransport-grease-error-code-limits.md) が扱う。0164 が `Session` API の値を 32 ビットに制限する方針を採る場合は本 issue のフォールバックが到達不能になるため、その場合の本 issue の扱い (closed にするかの判断を含む) は 0164 の決定時に行う。
- 採用した方針と理由をコメントに残す。

## 完了条件

- 32 ビットを超える close code (varint の範囲内) では CONNECTION_CLOSE 経路を選び、元のコードを載せる判定を純関数に切り出して単体テストで固定する。既存の `moqt_close_code_*` テストは新しい判定に合わせて更新し、32 ビットに収まる値が capsule 経路のままで切り捨てられないことも引き続き固定する。
- varint の上限 (2^62-1) を超える値 (`u64::MAX` など) でも接続を閉じる判定になること (コードは `Error::UNKNOWN` になる) を単体テストで固定する。
- 32 ビットに収まる close code の挙動 (`WT_CLOSE_SESSION` + FIN) が変わらないこと。
- `WtSession` が `Handle::close` を呼ぶ配線とセッション状態の遷移は s2n-quic の I/O ハンドルが必要で単体テストでは構築できないため、レビューで確認する。接続が実際に閉じることの実機確認は WebTransport セッションの確立自体が [issues/pending/0094](../issues/pending/0094-bug-webtransport-reset-stream-at-unsupported.md) の解消待ちであるため、その解消後に行う。
- 採用した方針と理由がコメントに残っていること。
- `make test` / `make clippy` / `make fmt` が通ること。

## 解決方法

`examples/tokio-moq/src/webtransport.rs` の `moqt_close_code` を「どの経路で接続へ伝えるか」を決める関数に変え、32 ビットに収まらない値は接続レベルの close に切り替えるようにした。

- `MoqtCloseCode` を追加し、`moqt_close_code(code: u64) -> MoqtCloseCode` が次の 3 つを返す
  - `Capsule(u32)`: 32 ビットに収まる。`WT_CLOSE_SESSION` capsule の Application Error Code として送る
  - `ConnectionClose(u64)`: varint の上限 (2^62-1) 以内。QUIC の `CONNECTION_CLOSE` に元のコードを載せる
  - `ConnectionCloseUnknown`: varint の上限を超える。`s2n_quic::application::Error::UNKNOWN` で接続を閉じる (コードは伝わらない)
- `examples/tokio-moq/src/webtransport_h3.rs` の `WtSession::close` は `MoqtCloseCode` を受け取り、`ConnectionClose` / `ConnectionCloseUnknown` では `s2n_quic::connection::Handle::close` で接続を閉じる。
  `WT_CLOSE_SESSION` の詳細メッセージは送れないが、draft-ietf-webtrans-http3-16 §6 は CONNECT stream の close もセッション終了の条件とするため接続レベルの close に切り替える。セッション状態は従来どおり送信前に終了へ移すため、
  新規ストリーム / datagram の拒否と既存ストリームの `WT_SESSION_GONE` での中断は同じように効く
- `examples/tokio-moq/src/transport.rs` の `StreamHandle::close` は判定結果を各経路へ渡す。WebTransport over HTTP/2 は接続レベルの close へのフォールバックを実装していないため、32 ビットに収まらない値は元のコードを含むエラーとして報告する (既存の挙動と同じ。`WtH2Session::close` の doc に明記した)

追加・更新したテスト:

- `examples/tokio-moq/src/webtransport.rs`: `moqt_close_code_accepts_32_bit_values` (`0` / `0x1` / `0x12` / `u32::MAX` が `Capsule` になる)、`moqt_close_code_falls_back_to_connection_close_over_32_bits` (`u32::MAX + 1` と greasing 値の上限が元のコードのまま `ConnectionClose` になる)
  、`moqt_close_code_uses_unknown_over_varint_limit` (`2^62` と `u64::MAX` が `ConnectionCloseUnknown` になる)
- `examples/tokio-moq/src/webtransport_h3.rs`: `moqt_close_code_accepts_values_within_32_bits` と `moqt_close_code_rejects_values_beyond_32_bits` を新しい判定に合わせて更新した

未実施の確認:

- `WtSession` が `Handle::close` を呼ぶ配線とセッション状態の遷移は s2n-quic の I/O ハンドルが必要で単体テストから構築できないため、レビューで確認した。接続が実際に閉じることの実機確認は WebTransport セッションの確立が `issues/pending/0094` の解消待ちであるため行っていない

`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。

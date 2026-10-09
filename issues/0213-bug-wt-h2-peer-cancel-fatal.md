# WebTransport over HTTP/2 で peer の cancel が example を終了させる

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-wt-h2-peer-cancel-fatal
- Polished: {YYYY-MM-DD}

## 目的

peer の STOP_SENDING (cancel) は該当する送信ストリームだけの終端であり、セッションと他のストリームの
配信 / 受信は継続すべきである (draft-ietf-moq-transport-22 §6.4.2.3 (Request Cancellation and Rejection))。
QUIC 直接接続と WebTransport over HTTP/3 (`wt-h3`) はこの扱いに対応しているが、WebTransport over
HTTP/2 (`wt-h2`) だけは peer の cancel で moq-pub / moq-sub が終了する。経路によって
「該当ストリームだけが終端する」と「プロセスが終了する」が分かれる非対称を解消する。

## 現状

- `--transport wt-h2` は moq-pub / moq-sub で選択できる (experimental 扱い)。
- `examples/tokio-moq/src/webtransport_h2.rs` は `WtEvent::StopSending { stream_id, error_code }` で
  wire のコードを受け取れるが、debug ログと `stop_sending_received` への記録、
  `fail_pending_sends` による未送信データの破棄だけを行う。`fail_pending_sends` は待っている送信へ
  `TransportError::StreamClosed` を返し、cancel 後の送信も拒否する。
- `examples/tokio-moq/src/moqt_client.rs` の `MoqtClient::establish_wt_h2` は `stop_sending_rx: None` を
  渡すため、wt-h2 では `MoqtClient::handle_stop_sending` が呼ばれず Session へ cancel が通知されない
  (subscription / fetch の状態が wt-h3 と揃わない)。
- `examples/moq-pub/src/error.rs` と `examples/moq-sub/src/error.rs` の
  `From<tokio_moq::error::TransportError> for Error` は `TransportError::StreamClosed` を明示せず
  catch-all で `WebTransport` にする。そのため pipeline の `is_peer_stream_reset`
  (`Error::StreamReset` のみ真) に該当せず、`close_on_message_error` + `main` の Fatal で終了する。
- 0152 は `From<TransportError>` の表示振り分けを網羅 match にする issue であり、本 issue は
  pipeline の挙動 (致命扱いをやめる) を扱う。目的が異なるため分けて扱う。

## 設計方針

- wt-h2 の `WtEvent::StopSending` を観測経路へ流し、`establish_wt_h2` でも peer の STOP_SENDING を
  Session へ通知できるようにする (wt-h3 と同じ扱い)。
- peer の cancel に由来する送信エラーを pipeline が「該当ストリームの終端」と判別できるようにする。
  `TransportError::StreamClosed` は cancel 以外の経路でも生成されるため、cancel 由来かを区別する方法
  (専用 variant にする / cancel を観測したストリーム id を台帳に持つ) を比較して決める。
- wt-h2 で cancel 後の送信がどのエラーで拒否されるかを確認し、該当ストリームの終端として扱えるようにする。
- wt-h2 が experimental であることと capsule 実装の挙動は変えない。

## 完了条件

- `--transport wt-h2` で peer が STOP_SENDING しても moq-pub / moq-sub が終了せず、該当ストリームだけが
  終端されて他のストリームの配信 / 受信が継続すること。
- wt-h2 でも peer の cancel が Session へ通知され、subscription / fetch の状態遷移が wt-h3 と同じに
  なること。
- 追加した判定を単体テストで固定すること (I/O ハンドルが必要な配線部分はレビューで確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること

# WebTransport over HTTP/2 で peer の cancel が example を終了させる

- Created: 2026-10-09
- Completed: 2026-10-12
- Branch: feature/fix-wt-h2-peer-cancel-fatal
- Polished: 2026-10-10
- Updated: 2026-10-10

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
  (`Error::StreamReset` のみ真) に該当せず、致命エラーとして扱われて example が終了する
  (moq-pub は `close_on_message_error` が `Error::Moqt` 以外を素通りしたうえで致命判定になり、
  moq-sub は `fatal_error` を立てて `main` を抜ける)。
- 0152 は `From<TransportError>` の表示振り分けを網羅 match にする issue であり、本 issue は
  pipeline の挙動 (致命扱いをやめる) を扱う。目的が異なるため分けて扱う。

## 設計方針

- wt-h2 の `WtEvent::StopSending` を観測経路へ流し、`establish_wt_h2` でも peer の STOP_SENDING を
  Session へ通知できるようにする (wt-h3 と同じ扱い)。
- peer の cancel に由来する送信エラーを pipeline が「該当ストリームの終端」と判別できるようにする。
  `TransportError::StreamClosed` は cancel 以外 (FIN / reset 済みなど) でも生成されるため、
  cancel 由来の失敗だけを既存の `TransportError::StreamReset` (0163 が QUIC / wt-h3 で確立した
  「peer が送信方向を終端した」の表現) で返す。driver の `stop_sending_received` を
  ストリーム id → エラーコードの台帳に拡張し、`fail_pending_sends` と `Command::Send` の
  拒否のうち台帳に一致するものだけを `StreamReset { error_code }` にする
  (wt-h2 の capsule は MOQT のコードをそのまま運ぶため remap は不要。
  変換則は `StopSendingTransport::WtH2` が実装済み)。
  新規 variant は追加しない (`From<TransportError>` に catch-all があるため写像の追記は強制されず、
  追加すると黙って `WebTransport:` 表示に落ちて 0152 が扱う表示振り分けと衝突する)。
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

## 解決方法

- `examples/tokio-moq/src/webtransport_h2.rs` の `DriverState::stop_sending_received` を
  ストリーム ID → エラーコードの台帳 (`HashMap`) に拡張し、台帳に載ったストリームへの送信の拒否と
  未送信データの失敗 (`fail_pending_sends`) を `TransportError::StreamReset { error_code }` に
  した。台帳に無い FIN / reset 済みのストリームは従来どおり `StreamClosed` のままである
  (写像は `stream_end_error` の 1 箇所に閉じた)。
- `WtH2Session` に STOP_SENDING の観測チャネルと `take_stop_sending_receiver` を追加し、
  `WtEvent::StopSending` で台帳の登録と観測の送出を行う。`establish_wt_h2` が観測 receiver を
  `finish_handshake` へ渡し、`MoqtClient::handle_stop_sending` 経由で Session へ cancel が届く
  ようにした (wt-h3 と同じ扱い)。capsule は MOQT のコードをそのまま運ぶため remap は不要である。
- テストは `examples/tokio-moq/src/webtransport_h2.rs` に追加した。実 TCP+TLS+HTTP/2 サーバーから
  `WT_STOP_SENDING` を送る `wt_h2_stop_sending_terminates_only_the_stream` で、未送信データの失敗と
  cancel 後の送信拒否が `StreamReset` になること、cancel 後も新しいストリームの送受信が継続する
  ことを固定した。`stream_end_error` の写像 (`None` → `StreamClosed`) と
  `take_stop_sending_receiver` の 1 回取り出し契約も単体テストで固定した。
- doc は `is_peer_stream_reset` (moq-pub / moq-sub)、`remap_stop_sending_error_code`、
  `StopSendingObserved`、`TransportError::StreamReset`、`skills/shiguredo-moqt/SKILL.md` の
  「wt-h2 は STOP_SENDING の観測経路を持たない / 致命扱いのまま」という記述を実装に合わせて
  更新した。
- 検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` の通過と、`/review-diff-code` 3 周 (致命的 0 件) で行った。
  peer cancel で example が終了しないことはパイプラインの終了経路の実トレースで確認した
  (実 relay での手動確認は未実施)。
- レビューで見つけた残りの課題は別 issue とした: 0243 (wt-h2 の `finish` / `reset` が driver の
  拒否を返さず Session の状態遷移が wt-h3 と異なる)、0244 (moq-pub の終了時の後始末が `Fatal`
  終了する)。

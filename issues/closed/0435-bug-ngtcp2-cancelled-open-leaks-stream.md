# ngtcp2 バックエンドで open 待ちがキャンセルされると開いたストリームが閉じられないまま残る

- Created: 2026-11-15
- Completed: 2026-11-15
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)

## 目的

`crates/tokio-moq/src/quic/ngtcp2.rs` の `retry_open_waiters` で、open 待ちの future が
破棄されていると、開いたストリームのハンドルがそのまま捨てられる。ハンドルを drop しても
FIN も RESET_STREAM も送られないため、ストリームは開いたままピアに残り、ピアが広告した
`MAX_STREAMS` の枠を消費し続ける。ストリームを確実に閉じる。

## 現状

- `retry_open_waiters` は待ち手を取り出し、`conn.open_uni_stream()` / `open_bidi_stream()` で
  ストリームを開いてから `oneshot::Sender::send` で結果を返す。送信に失敗した場合
  (呼び出し側の future が既に破棄されている場合) は `let _ = ...` で結果を捨てている。
- `SendStream` は `Drop` を実装しておらず、drop しても FIN / RESET_STREAM を送らない。このため、
  開いたストリームは双方向・単方向のどちらも、アプリからもピアからも使われないまま残る。
- 双方向ストリームでは `RecvStream` も捨てられるが、STOP_SENDING を送らないため、ピアは
  データを送り続けうる。
- 影響: 長時間動く example では open のキャンセルが積み重なり、`MAX_STREAMS` の枠を消費して
  新しい stream の open が待たされる。`quic-ngtcp2` ビルドでだけ起きる。
- 本 issue は open 側の窓に限る。受理側は別 issue で対応済み、datagram 受信は別 issue で扱う。

## 設計方針

- open の結果を待ち手へ送れなかった場合は、開いたストリームをその場で閉じる。
  - 単方向: `reset_stream(id, 0)` で RESET_STREAM を送る。アプリが 1 バイトも書いていない
    ストリームなので、エラーコードは NO_ERROR (0) にする。
  - 双方向: 送信方向を `reset_stream(id, 0)`、受信方向を `stop_sending(id, 0)` で閉じる。
- ストリームの ID は `SendStream` / `RecvStream` が保持しているため、失敗した値から取り出して
  閉じる。閉じたあとの drop は無害であり、将来 `SendStream` に `Drop` が入っても二重送出に
  ならない。
- 閉じる処理が失敗しても、待ち手は既に消えているため報告先が無い。警告ログを出すに留める。

## 完了条件

- open 待ちがキャンセルされた場合に、開いたストリームが RESET_STREAM (および双方向では
  STOP_SENDING) で閉じられること。
- 回帰テストがあること。受理待ちを 1 回だけ poll してから破棄し、そのあとに開いた stream が
  peer 側で終端として観測されることを確認する。
- `cargo test -p tokio-moq --lib --no-default-features --features quic-ngtcp2` が通ること。
- `cargo test -p moq-sub --lib --no-default-features --features quic-ngtcp2 peer_harness_integration_tests`
  が通ること。
- `cargo clippy --workspace --all-targets -- -D warnings` と `cargo fmt --all -- --check` が通ること。

## 解決方法

open の結果を待ち手へ渡せなかった場合に、開いたストリームをその場で閉じるようにした。

- `crates/tokio-moq/src/quic/ngtcp2.rs` の `retry_open_waiters` で、`oneshot::Sender::send` が
  失敗したときに受け取ったストリームを閉じる。単方向は `reset_stream(id, 0)` で RESET_STREAM を
  送り、双方向は送信方向を `reset_stream(id, 0)`、受信方向を `stop_sending(id, 0)` で閉じる。
  アプリが 1 バイトも書いていないストリームのため、エラーコードは NO_ERROR (0) にした。
- 閉じる処理そのものが失敗した場合は、警告ログ (`Failed to reset an unused outgoing stream` /
  `Failed to stop an unused incoming stream`) を出す。待ち手は既に消えているため報告先が無い。
- 回帰テスト `cancelled_open_closes_the_stream` を `crates/tokio-moq/src/peer_harness.rs` に
  追加した。open 待ちを 1 回だけ poll してから破棄し、ピア側で受理したストリームが
  `RequestStreamEnd::Reset { error_code: Some(0) }` で終わることを確認する。

変異確認: 閉じる処理を削除すると、この回帰テストが 3 回実行して 3 回とも失敗する
(5 秒の待ちで終端を観測できない)。修正後は 3 回とも成功する。

検証: `cargo test -p tokio-moq --lib --no-default-features --features quic-ngtcp2` /
`cargo test -p moq-sub --lib --no-default-features --features quic-ngtcp2 peer_harness_integration_tests` /
`cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check`。

なお、`SendStream` の drop で RESET_STREAM を送る案は ngtcp2 側の API 契約に関わるため、
本 issue ではストリームを明示的に閉じる形にした。将来 `SendStream` に `Drop` が入っても、
閉じたあとの drop は無害であり二重送出にならない。

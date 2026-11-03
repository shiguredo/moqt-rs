# 不正位置の WT_STREAM を H3_FRAME_UNEXPECTED にする

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-wt-h3-draft-17-wt-stream-placement-error
- Polished: 2026-11-03

## 目的

[draft-ietf-webtrans-http3-17 §4.3](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従する。

## 現状

`crates/tokio-moq/src/webtransport_h3.rs` の `feed_stream_to_h3` は依存の HTTP/3 エラーを伝搬する。
固定中の `shiguredo_http3 2026.1.0-canary.13` の `src/stream/control.rs` / `src/stream/request.rs` は、不正な位置の WT_STREAM (`0x41`) を FrameError として返す。
旧 request stream という表現に沿う先頭例外も残る。

## 設計方針

WT_STREAM が bidirectional stream の initiator が送る最初のバイト列でだけ認められる draft 17 の規則に対応する依存版を取り込む。
他の位置で受けたときは H3_FRAME_UNEXPECTED をそのまま接続終了へ伝搬させる。
正常な server-initiated bidi stream の先頭を、一律に request stream でないという理由で拒否しない。

## 完了条件

- SETTINGS 受信済み control stream、request の途中、responder の送信方向などの不正位置を検証すること。
- 正当な client / server 開始 bidi の先頭と、分割された先頭ヘッダを受理すること。
- adapter で別の汎用エラーに潰さず、依存側で規定された接続終了コードを保存すること。

## 依存・関連

- 関連: [0407](0407-wt-h3-draft-17-settings-version-negotiation.md)

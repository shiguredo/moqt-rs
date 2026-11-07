# 不正位置の WT_STREAM の H3_FRAME_UNEXPECTED を接続終了へ伝搬することを固定する

- Created: 2026-11-03
- Completed: 2026-11-08
- Branch: develop
- Polished: 2026-11-08

## 目的

[draft-ietf-webtrans-http3-17 §4.3](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従する。WT_STREAM (`0x41`) は bidirectional stream の initiator が送る最初のバイト列でだけ認められ、それ以外の位置で受けた場合は `H3_FRAME_UNEXPECTED` の接続エラーにする。

変更主体は依存クレートである。0407 で `shiguredo_http3` を canary.14 に更新すると、依存が不正位置の WT_STREAM を `H3_FRAME_UNEXPECTED` で返す。本 issue は adapter がその接続終了コードを保存して接続を閉じることを Sans I/O テストで固定し、§4.3 の参照コメントを draft-17 に更新する。production code の機能変更は行わない。

## 現状

0407 で `shiguredo_http3` を `2026.1.0-canary.14` に更新する。canary.14 の `src/stream/control.rs` は SETTINGS 受信後の制御ストリームで `0x41` を `ConnectionError(FrameUnexpected)` にし、`src/stream/request.rs` は initiator の先頭以外の `0x41` を `ConnectionError(FrameUnexpected)` にする。SETTINGS 受信前の制御ストリームは RFC 9114 §6.2.1 を優先して `H3_MISSING_SETTINGS` になる。

`crates/tokio-moq/src/webtransport_h3.rs` の `feed_stream_to_h3` は h3 層の結果を `process_h3_result` に渡す。`connection_error_code` が `Error::ConnectionError(code)` からコードを取り出し、`process_h3_result` が `close_connection_with_h3_error` に渡して接続を閉じる。呼び出し元へ返る値は `TransportError::ConnectionClosed` であり、コードは close の application error code として wire に渡る。

不正位置の WT_STREAM を受けたときに close のコードが `H3_FRAME_UNEXPECTED` のままであることを固定するテストは無い。§4.3 の参照コメントは draft-16 のままである。

## 設計方針

production code は変更しない。0407 の依存更新で §4.3 は満たされ、adapter の既存経路が接続エラーコードを close に渡す。

`crates/tokio-moq/src/webtransport_h3.rs` の `#[cfg(test)] mod tests` に Sans I/O テストを追加する。モックは使わず、`ClientConnectionState`、`peer_control_stream_bytes`、`ServerConnection` を使う。

- SETTINGS 受信後のサーバー制御ストリーム (`SERVER_CONTROL_STREAM_ID`) に `0x41` を流すと `ConnectionError(H3ErrorCode::FrameUnexpected)` になり、`connection_error_code` が `0x105` を返すこと。
- 同じ位置に `[0x40]` と `[0x41]` を 2 回に分けて流しても、可変長整数の確定後に `H3_FRAME_UNEXPECTED` になること。
- サーバー開始 bidi の先頭の `0x41` + session ID が拒否されないこと。これは `decide_route` の既存テストで受理側が固定されていることを確認し、実 QUIC ストリームを要求する `route_bi_stream` の追加テストは行わない。

テストの期待値は canary.14 の `H3ErrorCode::FrameUnexpected` (0x105) を定数で参照し、数値を再定義しない。

§4.3 の参照コメントは `open_bi_stream` と `route_bi_stream` の 2 箇所を draft-17 に更新する。モジュール doc、`wait_for_peer_settings`、テストヘルパー、`Cargo.*` は 0407 が所有するため変更しない。

## 完了条件

- SETTINGS 受信後のサーバー制御ストリームの `0x41` と、分割された `0x40` → `0x41` が `H3_FRAME_UNEXPECTED` の接続エラーになること。
- `connection_error_code` が `H3_FRAME_UNEXPECTED` (0x105) を返し、接続終了コードが別のコードに置き換わっていないこと。
- サーバー開始 bidi の先頭の `0x41` + session ID が受理されることが、既存の `decide_route` テストで確認できること。
- `crates/tokio-moq/src/webtransport_h3.rs` の `open_bi_stream` と `route_bi_stream` の §4.3 参照が draft-17 になっていること。
- `cargo test -p tokio-moq` で追加したテストが通ること。

## 差分の確認範囲

- 依存バージョンと `Cargo.toml` / `Cargo.lock` は 0407 が所有する。本 issue では変更しない。
- SETTINGS 受信前の制御ストリームは、RFC 9114 §6.2.1 を優先して `H3_MISSING_SETTINGS` になる。これは依存の判断であり、本 issue の `H3_FRAME_UNEXPECTED` の検証対象ではない。
- サーバー側の request 受信方向と response 送信方向は、このクライアント専用 adapter の責務外である。依存の `ServerConnection` を使うテストは CONNECT の確立に必要な範囲だけに使う。
- CONNECT 応答方向のバイト列は DATA であり HTTP/3 フレームではない。canary.14 は応答方向の `0x41` を `H3_FRAME_UNEXPECTED` にしないため、この方向はテスト対象外とする。
- サーバー開始 bidi のヘッダー確定後の本体はアプリケーションペイロードであり、HTTP/3 フレームではない。本体の `0x41` をフレームとして拒否する変更は行わない。
- §4.4 の Reliable Size は 0410 で追跡する。

## 依存・関連

- 先行: [0407](closed/0407-wt-h3-draft-17-settings-version-negotiation.md)

## 解決方法

SETTINGS 受信後のサーバー制御ストリームに WT_STREAM (`0x40 0x41`) を流すと `ConnectionError(FrameUnexpected)` になり、`connection_error_code` が `0x105` を返すことをテストで固定した。可変長整数が `0x40` と `0x41` に分割されて届く場合も、型の確定後に同じ接続エラーになることを確認した。

`open_bi_stream` と `route_bi_stream` の §4.3 参照を draft-17 に更新した。

CONNECT 応答方向のバイト列は DATA であり HTTP/3 フレームではないため、canary.14 は `0x41` を `H3_FRAME_UNEXPECTED` にしない。この方向はテスト対象外とし、issue の完了条件も実態に合わせた。

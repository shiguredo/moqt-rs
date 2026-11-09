# HTTP/2 の WT_MAX 系 capsule に厳密な上限増加を要求する

- Created: 2026-11-03
- Completed: 2026-11-08
- Branch: develop
- Polished: 2026-11-08

## 目的

[draft-ietf-webtrans-http2-16 §6.5 〜 §6.7](../refs/webtrans/draft-ietf-webtrans-http2-16.txt) の「初期値または前回受理した値より大きくない WT_MAX_DATA / WT_MAX_STREAM_DATA / WT_MAX_STREAMS を拒否する」変更に追従する。
初期値の根拠は §4.3 と §11.2 とする。

§6.4 の WT_STREAM が上限を超えた場合の `WT_FLOW_CONTROL_ERROR` は、依存の WtSession の受信データ検証に委ね、本 issue の対象外とする。

## 現状

`crates/tokio-moq/Cargo.toml` / `Cargo.lock` は `shiguredo_http2 2026.1.0-canary.17` を固定している。
canary.17 の `src/webtransport/flow_control.rs` の `WtFlowControl::update_send_max` / `update_max_streams` は、前回受理した値 (未受信なら SETTINGS 由来の初期値) を増加させない値を `WtErrorKind::FlowControlError` にする。
`src/webtransport/stream.rs` の `WtStream::update_send_max` も同値・減少を拒否し、`WtSession::process` は WT_MAX_STREAM_DATA について、暗黙生成するピア開始 bidi ストリームの初期値と削除済みストリームの最後の受理値に対しても非増加を拒否する。
削除済みストリームの記録は上限 (10,000 件) 付きであり、追い出された ID ではこの比較が成り立たない。
adapter は capsule の解析・状態管理を `WtSession::feed` / `process` に委譲し、`DriverState::handle_event` の `Event::DataReceived` で `WtError` を受けると `abort_session_with_wt_error` → `wt_http2_error_code` で CONNECT stream を `WT_FLOW_CONTROL_ERROR` の RST_STREAM にする。
この経路を実 peer で固定するテストは無く、単体テストは `wt_http2_error_code_maps_session_errors` の対応付けだけである。

## 設計方針

production code の変更は、受信した WT_MAX_* の増加で滞留中の送信を再開する `flush_pending_sends` の呼び出しだけに限定する。依存の canary.17 が draft-16 の厳密増加を実装しており、adapter の既存経路がそのエラーを CONNECT stream の reset に伝える。
`crates/tokio-moq/src/webtransport_h2.rs` の `#[cfg(test)] mod tests` に、自己署名証明書を使う実 HTTP/2 peer との結合テストを追加する。モックは使わない。
テスト peer の `WtSession::send_max_data` / `send_max_stream_data` / `send_max_streams` は送信側でも非増加を拒否するため、非増加の capsule は `shiguredo_http2::webtransport::CapsuleEncoder` で `Capsule::WtMax*` を符号化し、`io.conn.send_data` で CONNECT stream に送る。増加値は `WtSession::send_max_*` で送り、client 側の送信またはストリーム開設の成立で受理を観測する。
初期値 0 の WT_MAX_STREAM_DATA は、client が `open_bi_stream` / `open_uni_stream` で開いたストリーム ID をテストサーバーへ渡してから、その ID 宛てに送る。
双方向・単方向の上限を独立して検証する。
拒否はセッション終了を伴うため、拒否ケースごとに新しい接続を張る。

## 完了条件

- WT_MAX_DATA / WT_MAX_STREAM_DATA / WT_MAX_STREAMS のそれぞれで、初期値と同値・初期値が非 0 なら小さい値、更新済み値と同値・小さい値を拒否すること。
- 初期値 0、SETTINGS 由来の非 0 値、stream ごとの初期値で検証すること。増加値は受理され、セッションが継続すること。
  - session の初期値は `SETTINGS_WT_INITIAL_MAX_DATA` と WT_MAX_DATA の対応で検証する。
  - stream ごとの初期値は `SETTINGS_WT_INITIAL_MAX_STREAM_DATA_UNI` / `..._BIDI_LOCAL` / `..._BIDI_REMOTE` と WT_MAX_STREAM_DATA の対応で、uni と bidi を分けて検証する。
  - WT_MAX_STREAMS は双方向と単方向を独立して検証する。受理は `open_bi_stream` / `open_uni_stream` の成立で観測する。
- 拒否時に client が CONNECT stream を `ErrorCode::WtFlowControlError` で reset し、実 peer の `Event::StreamReset` でそのコードを観測できること。
- WT_MAX_STREAM_DATA は対象ストリームが存在してから送ること (§4.3.2)。存在しないストリームへの送信で `WT_STREAM_STATE_ERROR` になる経路と混同しないこと。

## 差分の確認範囲

- §6.4 の WT_STREAM が上限を超えた場合の `WT_FLOW_CONTROL_ERROR` は、依存の WtSession の受信データ検証に委ねる。本 issue では扱わない。
- テストの初期値は §4.3.1 と §11.2 の SETTINGS だけを根拠にする。現行 client は WebTransport-Init を送らないため、§4.3.2 の `u` / `bl` / `br` との max マージは対象外とする。
- §6.6 の「WT_STOP_SENDING 後の WT_MAX_STREAM_DATA は `WT_STREAM_STATE_ERROR`」は既存の依存実装が扱う。本 issue の厳密増加の拒否と混同しない。

## 依存・関連

- 関連: [0405](closed/0405-wt-h2-draft-16-settings-codepoint.md)

## 解決方法

canary.17 の WT_MAX_\* の厳密増加を、自己署名証明書を使う実 TLS peer との結合テストで固定した。production code は、受信した WT_MAX_* の増加で滞留中の送信を再開するため、`handle_event` の受信処理後に `flush_pending_sends` を呼ぶようにした。

- WT_MAX_DATA、WT_MAX_STREAM_DATA (uni / bidi remote / bidi local)、WT_MAX_STREAMS (uni / bidi) の初期値と更新値の同値・小さい値を拒否する
- WT_MAX_DATA / WT_MAX_STREAMS / WT_MAX_STREAM_DATA の増加を受理し、送信またはストリーム開設の成立で確認する
- 初期値は依存の既定値 (1 MiB / 256 KiB / 100) より大きい値を使い、SETTINGS の mapping 退行も検出する
- `build_limits` が 6 つの `SETTINGS_WT_INITIAL_MAX_*` を正しく写すことを単体テストで固定する
- テスト peer は `WtSession` の送信 API が非増加を拒否するため、`CapsuleEncoder` で capsule を組み立てて CONNECT stream に送る

# WebTransport 経路で 32 ビット超の MOQT エラーコードを送れない制限の扱いを決める

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/change-webtransport-grease-error-code-limits
- Polished: 2026-09-28
- Updated: 2026-10-10

## 目的

draft-ietf-webtrans-http3-16 §4.4 (Resetting Data Streams) と draft-ietf-webtrans-http2-15 §6.2 / §6.3 は、
WebTransport のストリーム操作 (RESET_STREAM / STOP_SENDING) のアプリケーションエラーコードを
32 ビット (0x00000000-0xffffffff) に限る一方、draft-ietf-moq-transport-22 §13 (Grease) の greasing 値
(`0x7f * N + 0x9D`、`0x9D, 0x11C, ..., 0x3fffffffffffffde`) は 32 ビットを超える値を取り得る。
§16.11.4 (Stream Reset Error Codes) の registry も greasing 予約を持つため、MOQT の reset コードとして
32 ビット超の値が正当にあり得る。

現状は QUIC 経路 (`moqt://`) では任意の varint (`2^62 - 1` 以下) を送れるが、WebTransport 経路
(`wt-h3` / `wt-h2`) では 32 ビット超の値を運ぶことができない。しかも経路ごとの挙動が
「QUIC は送信する / H3 は送信時にエラー / H2 は呼び出し元へエラーを返さずに送らない」と
3 通りに分かれており、
この非対称とエラーを受けた呼び出し元での扱いが決まっていない。本 issue は制限の扱い (下記
(a) / (b) / (c)) を選び、選んだ理由を記録する。

## 現状

- `examples/tokio-moq/src/webtransport_h3.rs` の `moqt_to_wt_code` は `u32::try_from` で 32 ビットに
  収まらない値を `TransportError::Internal` にし、エラーメッセージに制限 (0x00000000-0xffffffff) を明示する。
  `WtSendStream::reset` / `WtRecvStream::stop_sending` → `stream_application_error` の経路から呼ばれる。
  この変換は 0134 の実装で入ったもので、doc コメントに「この制限は §4.4 の u32 制限に由来し、
  `RequestStreamEnd` の型や greasing の扱いとは別に WebTransport 経路の既知の制限として扱う」と記録され、
  `moqt_to_wt_code_rejects_values_beyond_32_bits` (2^32 以上 u64::MAX) で固定されている
  (2^32 以上 2^62 未満は従来 remap せず wire に載っていたため、エラーへの変更は挙動変更である)。
- `examples/tokio-moq/src/webtransport_h2.rs` の `WtH2SendStream::reset` / `WtH2RecvStream::stop_sending` は
  `u64` をそのまま shiguredo_http2 の `reset_stream` / `stop_sending` へ渡す。依存側は 0xffffffff 超を
  `flow_control_error` で拒否する (draft-ietf-webtrans-http2-15 §6.2 / §6.3 の MUST NOT) が、example の
  driver はこのエラーを `debug` ログに残すだけで WT_RESET_STREAM / WT_STOP_SENDING を送らず、
  呼び出し元には `Ok(())` を返す。example 側の doc は `WtH2SendStream::reset` の
  「capsule は 32 ビットのコードを運ぶ」の記述にとどまり、`WtH2RecvStream::stop_sending` の doc も
  送信の目的のみで、0xffffffff 超の拒否・MUST NOT の根拠・エラーが呼び出し元へ返らないことは記されていない。
- `examples/tokio-moq/src/transport.rs` の `SendStream::reset` / `RecvStream::stop_sending` の QUIC 分岐は
  `s2n_quic::application::Error::new` にそのまま渡すため、`2^32` 以上 `2^62` 未満の値は QUIC 経路では
  送信できる (`Error::new` は varint の上限 `2^62 - 1` まで受け付ける)。
- `src/session/data.rs` の公開 API (`Session::reset_outgoing_data_stream_with_code` /
  `reset_outgoing_data_stream_at_with_code`) は任意の `u64` を受け付け、ローカル専用コード
  (`SESSION_LOCAL_FILTER_MISMATCH` / `SESSION_LOCAL_DATAGRAM_TIMEOUT`) を `STREAM_INTERNAL_ERROR` に
  置換する以外の範囲検証はしない。呼び出し元
  (`examples/moq-pub/src/stream_writer.rs` の `SubgroupWriter::finish`、
  `examples/tokio-moq/src/moqt_client.rs` の `SessionEvent::ResetRequestStream` 分岐など) も範囲を検証しない。
  現行 example が渡すのは `SubgroupTermination::reset_error_code()` / `DataStreamResetReason::Cancelled.error_code()`
  の MOQT §12.5 の小さい値だけなので、32 ビット超は公開 API 経由で送信時に初めて問題になる。
- close 経路 (MOQT §16.11.1 の Session Termination Error Codes) の 32 ビット超の扱いは本 issue の対象外であり、
  [issues/0176](../issues/closed/0176-bug-moqt-close-code-over-32-bits.md) (closed、2026-09-30) が対応済みである。

## 設計方針

- 次のいずれかを選び、選んだ理由を記録する (現行実装は H3 の挙動だけが (c) に近い状態にある)
  - (a) 制限を doc に書き、呼び出し元が 32 ビットを超えるコードを渡さない契約にする。
    書く場所は、トランスポート非依存の `Session` API に WebTransport 固有の制限を書くと
    齟齬が生じるため、
    `examples/tokio-moq/src/transport.rs` の `SendStream::reset` / `RecvStream::stop_sending` と
    呼び出し元 (moq-pub / moq-sub) の doc に分かれる。
  - (b) `Session` の API (`reset_outgoing_data_stream_with_code` / `reset_outgoing_data_stream_at_with_code`) で
    受け付ける値を 32 ビットに制限し、送信前にエラーにする (QUIC 経路の挙動も変わる)。
    接続レベルの close は `Session::close` と `moqt_close_code` の close 用の別経路であり (0176 で実装済み)、
    reset コードの制限とは独立している。
  - (c) 経路で扱いを分ける。QUIC 経路はそのまま送り、WebTransport 経路は「送れない」ことを
    `moqt_to_wt_code` のエラーメッセージのように明示する (H3 はこの挙動とテスト・doc が既にある)。
    (c) を採る場合、H2 は現状エラーを呼び出し元へ返さないため、それを変更対象にすること。
- 数値の判定は `u32::try_from` を `moqt_to_wt_code` の 1 箇所に保ち、QUIC 経路の varint 上限
  (`2^62 - 1`) の判定と混同しない (`s2n_quic::application::Error::new` の失敗と
  `moqt_to_wt_code` の失敗は別の制限である)。H2 は判定を依存 crate 側に委ねているため、
  (c) を採る場合はエラーの表面化をどうするかを決める。
- greasing 値の生成側の扱いを決める。`shiguredo_moqt::grease::generate(n)` は 32 ビット超の値を返し得る
  (上限 `0x3fffffffffffffde`) が、ライブラリがエラーコードの greasing 値を生成する箇所は無く、
  `grease::is_grease` は Property 型などの判定にのみ使われている。アプリが greasing 値を reset コードに
  使う場合に、WebTransport 経路へ渡る値をどのように 32 ビット内に保証するか (利用側の契約か、
  生成側の変更か) を決める。

## 完了条件

- 選んだ方針 (a) / (b) / (c) と理由、および (c) を採る場合の H2 の扱い、
  greasing 値の生成側の扱いが記録されていること
- 32 ビット超のコードを渡したときの挙動が経路ごとに文書化され、テストで固定されていること
  (H3 は `moqt_to_wt_code_rejects_values_beyond_32_bits` が既にある。(c) を採る場合、
  H2 でエラーが呼び出し元へ返ること、QUIC 経路で `2^62 - 1` まで送れることがそれぞれ対象になる)
- QUIC 経路と WebTransport 経路 (H3 / H2) の差が、選んだ方針と一致していること
- 32 ビット制限が §4.4 (H3)、§6.2 / §6.3 (H2) に由来する制限であることがコメント・doc に書かれていること
- (b) を選ぶ場合は [issues/0176](../issues/closed/0176-bug-moqt-close-code-over-32-bits.md) (closed) の
  close フォールバックと矛盾しないこと

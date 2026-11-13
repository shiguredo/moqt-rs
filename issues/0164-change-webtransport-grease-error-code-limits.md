# WebTransport 経路で 32 ビット超の MOQT エラーコードを送れない制限の扱いを決める

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/change-webtransport-grease-error-code-limits
- Polished: 2026-11-14
- Refreshed: 2026-11-04

## 目的

draft-ietf-webtrans-http3-17 §4.4 (Resetting Data Streams) と draft-ietf-webtrans-http2-16 §6.2 / §6.3 は、
WebTransport のストリーム操作 (RESET_STREAM / STOP_SENDING) のアプリケーションエラーコードを
32 ビット (0x00000000-0xffffffff) に限る一方、draft-ietf-moq-transport-23 §13 (Grease) の greasing 値
(`0x7f * N + 0x9D`、`0x9D, 0x11C, ..., 0x3fffffffffffffde`) は 32 ビットを超える値を取り得る。
§16.12.4 (Stream Reset Error Codes) の registry も greasing 予約を持つため、MOQT の reset コードとして
32 ビット超の値が正当にあり得る。

現状は QUIC 経路 (`moqt://`) では任意の varint (`2^62 - 1` 以下) を送れるが、WebTransport 経路
(`wt-h3` / `wt-h2`) では 32 ビット超の値を運ぶことができない。しかも経路ごとの挙動が
「QUIC は送信する / H3 は送信時にエラー / H2 の reset は送信時にエラー / H2 の stop_sending は
呼び出し元へエラーを返さずに送らない」と分かれており、この非対称とエラーを受けた呼び出し元での
扱いが決まっていない。本 issue は制限の扱い (下記 (a) / (b) / (c)) を選び、選んだ理由を記録する。

## 現状

- `crates/tokio-moq/src/webtransport_h3.rs` の `moqt_to_wt_code` は `u32::try_from` で 32 ビットに
  収まらない値を `TransportError::Internal` にし、エラーメッセージに制限 (0x00000000-0xffffffff) を明示する。
  `WtSendStream::reset` / `WtRecvStream::stop_sending` → `stream_application_error` の経路から呼ばれる。
  この変換は 0134 の実装で入ったもので、doc コメントに「この制限は §4.4 の u32 制限に由来し、
  `RequestStreamEnd` の型や greasing の扱いとは別に WebTransport 経路の既知の制限として扱う」と記録され、
  `moqt_to_wt_code_rejects_values_beyond_32_bits` (`u32::MAX + 1` と `u64::MAX` の 2 値) で固定されている
  (2^32 以上 2^62 未満は従来 remap せず wire に載っていたため、エラーへの変更は挙動変更である)。
- `crates/tokio-moq/src/webtransport_h2.rs` の `WtH2SendStream::reset` / `WtH2RecvStream::stop_sending` は
  `u64` をそのまま shiguredo_http2 の `reset_stream` / `stop_sending` へ渡す。依存側は 0xffffffff 超を
  `flow_control_error` で拒否する (draft-ietf-webtrans-http2-16 §6.2 / §6.3 の MUST NOT)。
  `WtH2SendStream::reset` は driver の `Command::Reset` が `wt_session.reset_stream` のエラーを
  `debug` ログに残し、WT_RESET_STREAM を送らずに `Err(TransportError::from(e))` を返すため、
  呼び出し元にもエラーが返る。`WtH2RecvStream::stop_sending` は `Command::StopSending` を送って
  `Ok(())` を返し、driver が `wt_session.stop_sending` のエラーを `debug` ログに残すだけで、
  呼び出し元は既に `Ok(())` を受け取っており、WT_STOP_SENDING は送られない。
  `crates/tokio-moq` 側の doc は `WtH2SendStream::reset` の「capsule は 32 ビットのコードを運ぶ」の
  記述にとどまり、`WtH2RecvStream::stop_sending` の doc も送信の目的のみで、0xffffffff 超の拒否・
  MUST NOT の根拠・エラーが呼び出し元へ返らないことは記されていない。
- `crates/tokio-moq/src/transport.rs` の `SendStream::reset` / `RecvStream::stop_sending` の QUIC 分岐は、
  選択された QUIC バックエンドの `reset` / `stop_sending` に `u64` をそのまま渡す。既定の tobi-quic は
  `VarInt::try_from` で、s2n-quic は `s2n_quic::application::Error::new` で、いずれも varint の上限
  `2^62 - 1` まで受け付けるため、`2^32` 以上 `2^62` 未満の値は QUIC 経路では送信できる。
- `src/session/data.rs` の公開 API (`Session::reset_outgoing_data_stream_with_code` /
  `reset_outgoing_data_stream_at_with_code`) は任意の `u64` を受け付け、ローカル専用コード
  (`SESSION_LOCAL_FILTER_MISMATCH` / `SESSION_LOCAL_DATAGRAM_TIMEOUT`) を `STREAM_INTERNAL_ERROR` に
  置換する以外の範囲検証はしない。呼び出し元
  (`crates/moq-pub/src/stream_writer.rs` の `SubgroupWriter::finish`、
  `crates/tokio-moq/src/moqt_client.rs` の `SessionEvent::ResetRequestStream` 分岐など) も範囲を検証しない。
  現行 example が渡すのは `SubgroupTermination::reset_error_code()` / `DataStreamResetReason::Cancelled.error_code()`
  の MOQT §12.5 の小さい値だけなので、32 ビット超は公開 API 経由で送信時に初めて問題になる。
- close 経路 (MOQT §16.12.1 の Session Termination Error Codes) の 32 ビット超の扱いは本 issue の対象外であり、
  [issues/0176](../issues/closed/0176-bug-moqt-close-code-over-32-bits.md) (closed、2026-09-30) が対応済みである。

## 設計方針

- 次のいずれかを選び、選んだ理由を記録する (現行実装は H3 の reset / stop_sending と H2 の reset が
  エラーを返し、H2 の stop_sending だけがエラーを返さない状態にある)
  - (a) 制限を doc に書き、呼び出し元が 32 ビットを超えるコードを渡さない契約にする。
    書く場所は、トランスポート非依存の `Session` API に WebTransport 固有の制限を書くと
    齟齬が生じるため、
    `crates/tokio-moq/src/transport.rs` の `SendStream::reset` / `RecvStream::stop_sending` と
    呼び出し元 (moq-pub / moq-sub) の doc に分かれる。
  - (b) `Session` の API (`reset_outgoing_data_stream_with_code` / `reset_outgoing_data_stream_at_with_code`) で
    受け付ける値を 32 ビットに制限し、送信前にエラーにする (QUIC 経路の挙動も変わる)。
    接続レベルの close は `Session::close` と `moqt_close_code` の close 用の別経路であり (0176 で実装済み)、
    reset コードの制限とは独立している。
  - (c) 経路で扱いを分ける。QUIC 経路はそのまま送り、WebTransport 経路は「送れない」ことを
    `moqt_to_wt_code` のエラーメッセージのように明示する。H3 はエラー挙動とテストが既にあり、
    `moqt_to_wt_code` の doc に 32 ビット制限が記録されているが、`WtSendStream::reset` /
    `WtRecvStream::stop_sending` の公開 doc には 32 ビット超の扱いが書かれていないため、公開 doc も対象になる。
    (c) を採る場合、H2 は reset では既にエラーを返すが、stop_sending は現状エラーを呼び出し元へ返さないため、
    stop_sending を変更対象にすること。reset のエラーが 32 ビット制限を明示しているかも確認する。
- トランスポート層の数値の判定は `u32::try_from` を `moqt_to_wt_code` の 1 箇所に保ち、QUIC 経路の
  varint 上限 (`2^62 - 1`) の判定と混同しない (`s2n_quic::application::Error::new` / tobi-quic の
  `VarInt::try_from` の失敗と `moqt_to_wt_code` の失敗は別の制限である)。H2 は判定を依存 crate 側に
  委ねているため、(c) を採る場合はエラーの表面化をどうするかを決める。
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
  H2 の stop_sending でエラーが呼び出し元へ返ること (reset は既に返る)、QUIC 経路で
  `2^62 - 1` まで送れることがそれぞれ対象になる)
- QUIC 経路と WebTransport 経路 (H3 / H2) の差が、選んだ方針と一致していること
- 32 ビット制限が §4.4 (H3)、§6.2 / §6.3 (H2) に由来する制限であることがコメント・doc に書かれていること
- (b) を選ぶ場合は [issues/0176](../issues/closed/0176-bug-moqt-close-code-over-32-bits.md) (closed) の
  close フォールバックと矛盾しないこと。0176 のフォールバックは close code 用の `StreamHandle::close` 経路であり、
  `Session::close` は任意の `u64` を受け付けるため、reset API の 32 ビット制限では到達不能にならない
  (0176 の「到達不能になる」記述は close code も 32 ビットに制限する場合の話である)。

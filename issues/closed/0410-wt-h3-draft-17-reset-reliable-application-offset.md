# RESET_STREAM_AT の Reliable Size にアプリ指定 offset を反映する

- Created: 2026-11-03
- Completed: 2026-11-08
- Branch: develop
- Polished: 2026-11-08

## 目的

[draft-ietf-webtrans-http3-17 §4.4](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従する。WebTransport の data stream を reset するときの `RESET_STREAM_AT` の Reliable Size を、WebTransport header 長にアプリケーションが指定した offset を加えた値にする。

対象は WebTransport over HTTP/3 (tobi-quic) の送信側である。`SETTINGS_WT_ENABLED` の codepoint、版選択、`reset_stream_at` transport parameter の広告は 0407 が所有する。
0094 は旧 `examples/moq-transport` の WebTransport over HTTP/3 経路が s2n-quic を使っていた頃の課題である。現行の WtH3 は tobi-quic に移行しており、本 issue の前提ではない。0094 の refresh / close は別途判断する。

## 現状

WtH3 は tobi-quic を使い、ピアが `reset_stream_at` を広告していれば `WtSendStream::reset_with` から `reset_stream_at(error_code, header_len)` を送る。Reliable Size は WebTransport header 長だけであり、アプリケーションの offset は反映されない。

`crates/tokio-moq/src/webtransport_h3.rs` の `WtSendStream` は送信済みデータ長を保持していない。`header_len` は送信側では `open_uni_stream` / `open_bi_stream` が実際に送ったヘッダ長を持つが、サーバー開始 bidi を受理した `route_bi_stream` ではピアが送ったヘッダ長を入れている。responder のクライアントはそのストリームに WebTransport header を送っていないため、この値は Reliable Size に使えない。

`crates/tokio-moq/src/transport.rs` の `SendStream::reset` と `WtSendStream::reset` は error code だけを受け取る。
`SessionEvent::ResetDataStream` は `reliable_size` を持つが、`crates/moq-pub/src/pipeline.rs` の `ResetDataStream` 処理は `..` で値を捨て、`SubgroupWriter::reset_by_session` は `reliable_size: None` で `send_data_stream_closed` を呼ぶ。

`Session::reset_outgoing_data_stream_at*` は `ResetDataStream` を発行する前に Session 側の stream 追跡を除去する。
そのため、このイベントを受けた I/O 層が `send_data_stream_closed` を呼ぶと `SESSION_PROTOCOL_VIOLATION` になる。
一方、delivery timeout の `tick_subscription_timeouts` は追跡を保持したまま `ResetDataStream` を発行し、I/O 層の `send_data_stream_closed` を待つ。両者を区別しないと、app 発 reset では `SESSION_PROTOCOL_VIOLATION` になり、delivery timeout では PUBLISH_DONE の flush が止まる。

`Some(n)` を発行する production 側の呼び出しは無く、raw `Session` の `reset_outgoing_data_stream_at*` を使う利用者だけが発行できる。

## 設計方針

### `reliable_size` の基準

`SessionEvent::ResetDataStream::reliable_size` の `Some(n)` は、MOQT stream の先頭からのバイト数とする。MOQT header を含み、WebTransport header を含まない。`None` はアプリケーションの offset 指定が無いことを表す。

受信側の `RequestStreamEnd::Reset::reliable_size` は wire の Reliable Size (WebTransport header を含む) のままとする。送信側の MOQT 基準と受信側の wire 基準を doc で区別する。

### WtH3

`WtSendStream` に送信済みデータ長 (`sent_data_len`) を追加し、`send` の成功時に加算する。Final Size は `header_len + sent_data_len` である。

- `Some(n)`: ピアが `reset_stream_at` を広告している場合だけ、`header_len.checked_add(n)` を Reliable Size にする。`n <= sent_data_len` と varint 上限 (`2^62 - 1`) を送出前に検査する。ピアが広告していない場合は `TransportError::Unsupported` を返し、黙って `RESET_STREAM` にフォールバックしない。
- `None`: ピアが広告している場合は `header_len`、していない場合は従来の `RESET_STREAM` を使う。
- サーバー開始 bidi を受理した `WtSendStream` の `header_len` は 0 にする。responder は WebTransport header を送っていないためである。クライアント開始 bidi は `open_bi_stream` が送ったヘッダ長を使う。
- `header_len` の doc を「この端点がそのストリームの送信方向で実際に送った WebTransport header 長」に直す。

### transport ごとの `Some(n)`

`SendStream::reset(error_code, reliable_size: Option<u64>)` にシグネチャを変更し、次のように扱う。

- WtH3: 上記のとおり `header_len + n`。`None` は header 長。
- WtH2: `Some(n)` は `TransportError::Unsupported`。draft-16 §6.2 は `WT_RESET_STREAM` の Reliable Size が送信済み `WT_STREAM` の総バイト数に等しいことを MUST とするため、アプリ指定 offset を表現できない。`None` は従来どおり。
- Quic: `Some(n)` は `TransportError::Unsupported`。native QUIC の `RESET_STREAM_AT` はバックエンド依存であり、本 issue では公開しない。`None` は従来どおり。
- Qmux: `Some(n)` は `TransportError::Unsupported`。`None` は従来どおり。

`TransportError::Unsupported(String)` を追加し、`is_retryable_connect_error` では再試行不可にする。未対応 transport の判定は `SendStream::reset` のディスパッチ前に行い、WtH3 のピア広告の判定は `WtSendStream::reset_with` で行う。

`Unsupported` を返した場合、要求された error code と Reliable Size の `RESET_STREAM_AT` は送らない。ストリームを drop したときの tobi-quic の後始末として通常の `RESET_STREAM` が送出され得るが、これはアプリ要求の代替ではなく、要求を満たせなかった結果として扱う。ストリームを開いたまま保持して後始末を遅らせる設計は取らない。

### パイプライン

`SessionEvent::ResetDataStream` に `tracking: ResetTracking` を追加する。`ResetTracking::Removed` は app 発 reset を表し、Session は追跡を除去済みで I/O 層は wire の reset だけを行う。`ResetTracking::Retained` は delivery timeout などを表し、I/O 層は wire の reset 後に `send_data_stream_closed` を呼ぶ。

`crates/moq-pub/src/pipeline.rs` の `ResetDataStream` 処理で `tracking` と `reliable_size` を取り出し、`reset_open_subgroup_stream` に渡す。
`SubgroupWriter::reset_by_session(error_code, reliable_size, tracking)` は `Retained` のときだけ `send_data_stream_closed` を呼ぶ。`SubgroupWriter::finish` の reset はアプリ offset を持たないため `None` を渡し、従来どおり通知する。

`SendStream::reset` の他の呼び出し元 (`moqt_client` の `StopSending` / `ResetRequestStream` 経路) は `None` を渡す。bidi request stream の reset は `ResetRequestStream` に `reliable_size` が無いため、本 issue の対象外である。

### 変更対象

- `crates/tokio-moq/src/transport.rs` の `SendStream::reset` と doc (未対応 transport の `Some(n)` はディスパッチ前に拒否し、WtH2 の `reset` シグネチャは変更しない)
- `crates/tokio-moq/src/error.rs` の `TransportError::Unsupported` と `crates/tokio-moq/src/lib.rs` の `is_retryable_connect_error`
- `crates/tokio-moq/src/webtransport_h3.rs` の `WtSendStream` (`sent_data_len` / `header_len` / `reset` / `reset_with`)、`route_bi_stream`、§4.4 の参照コメント
- `crates/tokio-moq/src/moqt_client.rs` の `SendStream::reset` 呼び出し 5 箇所 (`None`) と、終端通知の責務に関するコメント
- `crates/moq-pub/src/pipeline.rs` の `ResetDataStream` 処理と `reset_open_subgroup_stream`
- `crates/moq-pub/src/stream_writer.rs` の `reset_by_session` / `finish`
- `src/session/types.rs` の `ResetTracking` と `ResetDataStream`、`src/session/data.rs` の `reset_outgoing_data_stream_at*` と `send_data_stream_closed`、`src/session/subscription/dispatch.rs` の delivery timeout reset
- `src/subgroup_tracker.rs` の `SubgroupStreamState::Reset` の doc
- `tests/test_session/subscription/request_update.rs` の delivery timeout の PUBLISH_DONE flush テスト
- `docs/missing-features.md` の `wt-h3` の `RESET_STREAM_AT` 記述 (87 行目と 520 行目)

## 完了条件

- 純関数 `absolute_reliable_size` で Reliable Size を計算し、`Some(0)` / `Some(sent_data_len)` / `Some(sent_data_len + 1)`、header 長 0〜9、varint 上限の各ケースを検証すること。境界エラーは `Unsupported` と区別した `TransportError` を返すこと。
- WtH3 の送信経路が `absolute_reliable_size` の結果を `reset_stream_at` に渡すこと。実 QUIC で Reliable Size を観測するハーネスは追加せず、wire 送出は tobi-quic の `reset_stream_at` の責務とする。
- サーバー開始 bidi を受理したストリームの `WtSendStream` は `header_len: 0` で構築し、ピアのヘッダ長を送信側の Reliable Size に加算しないこと。
- ピアが `reset_stream_at` を広告していない場合の `Some(n)` と、WtH2 / Quic / Qmux の `Some(n)` が `TransportError::Unsupported` になり、要求された `RESET_STREAM_AT` を送らないこと。ストリームの drop に伴う通常の `RESET_STREAM` は許容する。
- `ResetDataStream { reliable_size: Some(n), tracking: Removed }` から pipeline / writer / transport まで `Some(n)` が保持され、app 発 reset で `SESSION_PROTOCOL_VIOLATION` にならないこと。
- `ResetDataStream { tracking: Retained }` の delivery timeout で `send_data_stream_closed` が呼ばれ、保留中の PUBLISH_DONE (UPDATE_FAILED) が従来どおり flush されること。
- `crates/tokio-moq/src/webtransport_h3.rs` の §4.4 参照と `docs/missing-features.md` が現在の実装に一致すること。

## 差分の確認範囲

- `SETTINGS_WT_ENABLED` の codepoint、版選択、`reset_stream_at` transport parameter の広告は 0407 で追跡する。本 issue では `Cargo.*` を変更しない。
- 旧 `examples/moq-transport` の s2n-quic 経路の `RESET_STREAM_AT` 非対応 (0094) は別課題である。現行 WtH3 は tobi に移行しており、本 issue の `Some(n)` は native QUIC では `Unsupported` にする。
- WtH2 の `WT_RESET_STREAM` は Reliable Size が送信済み総バイト数に等しい MUST のため、アプリ offset を表現しない。`Some(n)` を拒否する。
- bidi request stream の reset (`ResetRequestStream`) は `reliable_size` を持たない。本 issue では `None` を渡し、API 拡張は行わない。
- 実 QUIC で Reliable Size を観測する WtH3 ハーネスは追加しない。Reliable Size の計算は純関数で固定し、wire 送出は tobi-quic の `reset_stream_at` (依存 crate のテストで検証済み) に委譲する。

## 依存・関連

- 先行: [0407](closed/0407-wt-h3-draft-17-settings-version-negotiation.md)
- 関連: [0094](pending/0094-bug-webtransport-reset-stream-at-unsupported.md) (旧 s2n-quic の WtH3 経路の課題。現行 WtH3 は tobi に移行済み)

## 解決方法

`SessionEvent::ResetDataStream` に `tracking: ResetTracking` を追加し、app 発 reset (`Removed`) と配送 timeout (`Retained`) を区別した。
`SubgroupWriter::reset_by_session(error_code, reliable_size, tracking)` は `Retained` のときだけ `send_data_stream_closed` を呼び、app 発 reset で `SESSION_PROTOCOL_VIOLATION` にならず、配送 timeout では保留 PUBLISH_DONE が従来どおり flush される。

`SendStream::reset(error_code, reliable_size: Option<u64>)` に変更し、WtH3 は `header_len + reliable_size` を `RESET_STREAM_AT` の Reliable Size として送る。
`WtSendStream` は `sent_data_len` を保持し、Final Size を `header_len + sent_data_len` とする。サーバー開始 bidi を受理した `WtSendStream` は `header_len: 0` にした。
`Some(n)` は `n <= sent_data_len` と varint 上限を検査し、ピアが `reset_stream_at` 非対応なら `Unsupported` を返す。`None` は従来どおりヘッダー長までを Reliable Size にする。

`TransportError::Unsupported` を追加し、WtH2 / QUIC / QMux の `Some(n)` はディスパッチ前に拒否する。`moqt_client` の request stream reset は `None` を渡す。`docs/missing-features.md` の RESET_STREAM_AT の記述を更新した。

Reliable Size の計算は純関数 `absolute_reliable_size` で固定し、header 長 0〜9、offset 0、送信済み全長、超過、varint 上限をテストした。実 QUIC で wire を観測するハーネスは追加していない。

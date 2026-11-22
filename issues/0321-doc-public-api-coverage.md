# 公開 API の doc を整備する

- Created: 2026-10-19
- Completed: 2026-11-22
- Branch: feature/update-doc-public-api-coverage
- Polished: 2026-11-20

## 目的

shiguredo-rust は公開 API (型・関数・フィールド・variant) に `///` を必ず書くことを求める。本 issue ではライブラリ本体 (`shiguredo_moqt`) と workspace の crate `tokio-moq` の公開 API を対象にする。現状は公開項目に doc が無い箇所と、doc が実装より古い箇所がある。利用者が rustdoc だけで契約を理解できる状態にする。

## 現状

- `RUSTDOCFLAGS="-W missing_docs" cargo doc --no-deps -p shiguredo_moqt -p tokio-moq` で、
  2026-11-20 時点で `shiguredo_moqt` 1 件 (`src/session/core.rs` の
  `Session::peer_auth_token_cache`)、`tokio-moq` 23 件の欠落が検出される。
  `tokio-moq` の内訳は `error.rs` 3 件、`moqt_client.rs` 5 件、`transport.rs` 8 件、
  `webtransport_h2.rs` 1 件、`webtransport_h3.rs` 6 件である。
- `crates/tokio-moq/src/moqt_client.rs` の `SubscribeResult` の `request_id` / `track_alias`、
  `StreamRead` の `Value` / `Closed`、`MoqtClient::stop_sending` に doc が無い。
  `ControlStream::new` には doc がある。
- `src/session/core.rs` のモジュール doc が兄弟モジュールとして `subscription.rs` /
  `fetch.rs` / `namespace.rs` / `goaway.rs` を挙げるが、`track_status.rs` が無い。
  メッセージ種別の列挙も `SUBSCRIBE / PUBLISH / FETCH / NAMESPACE 系 / GOAWAY` であり、
  TRACK_STATUS が無い。`namespace.rs` は存在し、NAMESPACE 系の request
  (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) と応答
  (NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED) を扱っている。
- `src/playout/buffer.rs` の `PlayoutBuffer::playout_delay_us` と
  `src/playout/timeline.rs` の `PlayoutTimeline::playout_delay_us` は同名だが、
  前者はトラックに応じて `PlayoutTimeline::presentation_extra_delay_us` (音声) または
  `PlayoutTimeline::video_delay_us` (映像) を返し、後者は音声の jitter buffer の遅延
  (上限適用後、`targetLatency` と同期制御の下限を含まない) を返す。
  doc はそれぞれの意味を書いているが、同名メソッド同士の相互参照が無く、
  名前から違いを判断できない。

## 設計方針

- 公開 API の doc 欠落を補う。`src/` と `crates/tokio-moq/src/` の公開項目を全数確認し、`///` の無い項目を補う。`RUSTDOCFLAGS="-D missing_docs" cargo doc --no-deps -p shiguredo_moqt -p tokio-moq` が通ることを確認する。
- 実装と食い違う doc と、実装より古い doc を修正する。`src/session/core.rs` の兄弟モジュール一覧に `track_status.rs` を追加し、メッセージ種別の列挙に TRACK_STATUS を追加する。`namespace.rs` は既に記載されており、NAMESPACE 系の送受信処理も実装済みである。
- 同名で意味が違う `PlayoutBuffer::playout_delay_us` と `PlayoutTimeline::playout_delay_us` は、本 issue では doc に相互参照と違いを明記して区別する。名前変更は破壊的変更であり本 issue の対象外とする。
- `tokio-moq` の rustdoc には intra-doc link 警告が 2026-11-20 時点で 8 件 (private item へのリンク 7 件、未解決リンク 1 件) あるが、本 issue は欠落 doc の補完と記述の修正を対象とし、リンク警告の解消は対象外とする。

## 完了条件

- `RUSTDOCFLAGS="-D missing_docs" cargo doc --no-deps -p shiguredo_moqt -p tokio-moq` が通ること。
- `src/session/core.rs` のモジュール doc の兄弟モジュール一覧に `track_status.rs` が含まれ、メッセージ種別の列挙に TRACK_STATUS が含まれていること。
- `src/playout/buffer.rs` と `src/playout/timeline.rs` の `playout_delay_us` の doc が相互参照と意味の違いを明記していること。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` と `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `RUSTDOCFLAGS="-W missing_docs" cargo doc --no-deps -p shiguredo_moqt -p tokio-moq` の
  欠落を 0 件にした。
  - `tokio-moq` の `TransportError` のフィールド、`Result` 型、`SubscribeResult` の
    `request_id` / `track_alias`、`StreamRead` / `RecvChunk` / 各 transport enum の variant、
    `webtransport_h2` / `webtransport_h3` の `ClientConfig` とメソッドに doc を追加した。
- 公開 doc から非公開項目への intra-doc link をコード表記に直し、
  `RUSTDOCFLAGS="-D warnings" cargo doc` を通した。
- `PlayoutBuffer::playout_delay_us` と `PlayoutTimeline::playout_delay_us` の doc に
  相互参照と意味の違いを明記した。
- `src/session/core.rs` のモジュール doc に `track_status.rs` と TRACK_STATUS を追加した。

`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo fmt --all -- --check`、`RUSTDOCFLAGS="-D warnings" cargo doc` が成功した。

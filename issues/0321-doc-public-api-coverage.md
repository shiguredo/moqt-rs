# 公開 API の doc を整備する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-public-api-coverage
- Polished: 2026-10-20

## 目的

shiguredo-rust は公開 API (型・関数・フィールド・variant) に `///` を必ず書くことを求める。本 issue ではライブラリ本体 (`shiguredo_moqt`) と workspace の example crate `tokio-moq` の公開 API を対象にする。現状は公開項目に doc が無い箇所、doc が別の項目に付いている箇所、doc が実装より古い箇所がある。利用者が rustdoc だけで契約を理解できる状態にする。

## 現状

- `src/object_properties.rs` の `find_varint` に `///` が無く、直前の `count_instances` の doc に「偶数型 Object Property から varint 値を取得する」という find_varint 向けの説明が混在している。
- `examples/tokio-moq/src/moqt_client.rs` の `ControlStream::new` と `SubscribeResult` の `request_id` / `track_alias` に doc が無い。
- `RUSTDOCFLAGS="-W missing_docs" cargo doc --no-deps -p shiguredo_moqt -p tokio-moq` で、
  2026-10-20 時点で `shiguredo_moqt` 1 件 (`find_varint`)、`tokio-moq` 33 件の欠落が検出される。
  `tokio-moq` の内訳は `error.rs` 3 件、`moqt_client.rs` 5 件、`transport.rs` 17 件、
  `webtransport_h2.rs` 1 件、`webtransport_h3.rs` 7 件であり、上記の 3 件はその一部である。
- `src/message_parameter.rs` の `LocationFilterContext::Fetch` の doc が「現状の呼び出しは subscription のみ」と書くが、`src/session/fetch.rs` と `src/session/subscription/fill.rs` から呼ばれている。
- `src/session/core.rs` のモジュール doc が存在しない `namespace.rs` を兄弟モジュールとして
  挙げている。さらに `NAMESPACE 系` の送受信処理を兄弟モジュールに分離していると書くが、
  NAMESPACE 系の request (PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS) は
  未対応であり、`core.rs` の `handle_unsupported_request` が `REQUEST_ERROR(NOT_SUPPORTED)`
  で拒否するのみである。応答専用の NAMESPACE / NAMESPACE_DONE / PUBLISH_SKIPPED は
  `PROTOCOL_VIOLATION` で閉じる。実際の兄弟モジュールは `subscription.rs` / `fetch.rs` /
  `track_status.rs` / `goaway.rs` である。
- `src/playout/buffer.rs` の `PlayoutBuffer::playout_delay_us` と `src/playout/timeline.rs` の `PlayoutTimeline::playout_delay_us` は同名だが、前者はそのトラックの表示の遅れ (`presentation_delay_us`)、後者は音声の jitter buffer の遅延 (上限適用後、`targetLatency` と同期制御の下限を含まない) を返す。doc はそれぞれの意味を書いているが、相互参照が無く、名前から違いを判断できない。
- `src/lib.rs` の `pub mod playout` の `///`、`src/playout.rs` の `//!`、`src/playout/scheduler.rs` の `//!` が「時間圧縮・伸長」「時間伸長」と書くが、`src/playout/stretch.rs` は時間伸長を持たないと明記している (`expand` は削除済み)。

## 設計方針

- 公開 API の doc 欠落を補い、別項目に付いている doc を正しい位置へ移す。`src/object_properties.rs` の `find_varint` の doc を `count_instances` から分離し、`count_instances` にはインスタンス数の説明だけを残す。
- 実装と食い違う doc を修正する。`LocationFilterContext::Fetch` の「現状の呼び出しは
  subscription のみ」を削除し、`src/session/core.rs` の兄弟モジュール一覧を実際の
  `subscription.rs` / `fetch.rs` / `track_status.rs` / `goaway.rs` に合わせ、NAMESPACE 系が
  未対応であることを明記する。`src/lib.rs` / `src/playout.rs` / `src/playout/scheduler.rs` の
  時間伸長の記述を、`src/playout/stretch.rs` の「時間伸長を持たない」に合わせる。
- 同名で意味が違う `PlayoutBuffer::playout_delay_us` と `PlayoutTimeline::playout_delay_us` は、本 issue では doc に相互参照と違いを明記して区別する。名前変更は破壊的変更であり本 issue の対象外とする。
- 上記の既知の欠落に加え、`src/` と `examples/tokio-moq/src/` の公開項目を全数確認し、`///` の無い項目を補う。`RUSTDOCFLAGS="-D missing_docs" cargo doc --no-deps -p shiguredo_moqt -p tokio-moq` が通ることを確認する。
- `tokio-moq` の rustdoc には private item への intra-doc link 警告が 2026-10-20 時点で 5 件あるが、本 issue は欠落 doc の補完と記述の修正を対象とし、リンク警告の解消は対象外とする。

## 完了条件

- `RUSTDOCFLAGS="-D missing_docs" cargo doc --no-deps -p shiguredo_moqt -p tokio-moq` が通ること。
- `src/object_properties.rs` の `find_varint` / `count_instances`、
  `src/message_parameter.rs` の `LocationFilterContext::Fetch`、`src/session/core.rs` の
  モジュール doc、`src/playout/buffer.rs` と `src/playout/timeline.rs` の `playout_delay_us`、
  `src/lib.rs` / `src/playout.rs` / `src/playout/scheduler.rs` の時間伸長の記述が実装と
  一致していること。
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` と `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

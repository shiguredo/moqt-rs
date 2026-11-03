# RESET_STREAM_AT の Reliable Size にアプリ指定 offset を反映する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-wt-h3-draft-17-reset-reliable-application-offset
- Polished: 2026-11-03

## 目的

[draft-ietf-webtrans-http3-17 §4.4](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従する。

## 現状

`crates/tokio-moq/src/webtransport_h3.rs` の `WtSendStream::reset` / `reset_with` と `crates/tokio-moq/src/transport.rs` の `SendStream::reset` は error code だけを受け取る。
`src/session/types.rs` の `SessionEvent::ResetDataStream` は `reliable_size` を表現するが、transport は RESET_STREAM_AT 非対応のフォールバックを持つ。
この基本的な非対応は pending の 0094 で追跡されている。

## 設計方針

0094 で実際の RESET_STREAM_AT と transport parameter の対応が可能になった後、アプリケーションの data offset に、その stream で実際に送った WebTransport header 長を加えた Reliable Size を設定する。
単に header 長以上という旧条件に留めない。
`SessionEvent::ResetDataStream::reliable_size` の `Some(n)` は、MOQT stream の先頭からのバイト数とし、MOQT header を含み WebTransport header を含まない。WT 層だけが実際に送った WT header 長を 1 回加算する。
`None` はアプリ offset の指定なしを表し、WT では offset 0 として WT header 分を Reliable Size に含める。他 transport の既存の `None` の意味は変えない。
加算時の u64 overflow、QUIC varint の上限 (`2^62 - 1`)、Reliable Size が Final Size を超えないこと、および header 長の可変性を検証する。
`crates/moq-pub/src/pipeline.rs` の `ResetDataStream` 処理 / `reset_open_subgroup_stream`、`crates/moq-pub/src/stream_writer.rs` の `SubgroupWriter::reset_by_session` から `transport::SendStream::reset` / `WtSendStream::reset` / `reset_with` まで offset を失わず渡し、Session へ返す終端通知でも同じ MOQT 基準の値を保持する。
明示された `Some(n)` を扱えない transport は送出前にエラーを返す。
0094 の基礎 transport 対応を重複実装する課題にはしない。

## 完了条件

- アプリ offset 0 / 非 0、uni / bidi、長さの異なる session ID ヘッダで wire の Reliable Size が一致すること。
- header 分を二重加算せず、範囲外 offset を送出前に拒否すること。
- `ResetDataStream` から pipeline / writer / transport まで `Some(n)` が保持され、WT の `None` は header 分を保護すること。
- 未対応 transport で明示された `Some(n)` を無視して成功扱いにせず、0094 の制約と整合すること。

## 依存・関連

- 先行: [0094](pending/0094-bug-webtransport-reset-stream-at-unsupported.md)
- 関連: [0407](0407-wt-h3-draft-17-settings-version-negotiation.md)

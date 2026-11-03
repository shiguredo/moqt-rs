# RESET_STREAM_AT の Reliable Size にアプリ指定 offset を反映する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-wt-h3-draft-17-reset-reliable-application-offset
- Polished: {YYYY-MM-DD}

## 目的

[draft-ietf-webtrans-http3-17 §4.4](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従する。

## 現状

`crates/tokio-moq/src/webtransport_h3.rs` の `WtSendStream::reset` / `reset_with` と `crates/tokio-moq/src/transport.rs` の `SendStream::reset` は error code だけを受け取る。
`src/session/types.rs` の `SessionEvent::ResetDataStream` は `reliable_size` を表現するが、transport は RESET_STREAM_AT 非対応のフォールバックを持つ。
この基本的な非対応は pending の 0094 で追跡されている。

## 設計方針

0094 で実際の RESET_STREAM_AT と transport parameter の対応が可能になった後、アプリケーションの data offset に、その stream で実際に送った WebTransport header 長を加えた Reliable Size を設定する。
単に header 長以上という旧条件に留めない。
u64 overflow と header 長の可変性を検証し、MOQT 側の offset が WT header を含むかを API 契約で明確にする。
0094 の基礎 transport 対応を重複実装する課題にはしない。

## 完了条件

- アプリ offset 0 / 非 0、uni / bidi、長さの異なる session ID ヘッダで wire の Reliable Size が一致すること。
- header 分を二重加算せず、範囲外 offset を送出前に拒否すること。
- 未対応 transport で指定 offset を無視して成功扱いにせず、0094 の制約と整合すること。

## 依存・関連

- 先行: [0094](pending/0094-bug-webtransport-reset-stream-at-unsupported.md)
- 関連: [0407](0407-wt-h3-draft-17-settings-version-negotiation.md)

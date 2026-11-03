# プロトコル識別子を moqt-23 に更新する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-protocol-identifier
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §6.2、§9.1](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`crates/tokio-moq/src/lib.rs` の `MOQT_PROTOCOL` は `moqt-22` であり、QUIC / QMux の ALPN と WebTransport のサブプロトコル交渉で共用している。
`webtransport_h2.rs` の `verify_wt_protocol` と対応テストにも draft 22 のリテラルがある。

## 設計方針

draft 23 のワイヤ対応が揃った時点で識別子を `moqt-23` に変更する。
`crates/tokio-moq/src/quic.rs` / `qmux.rs` / `webtransport_h2.rs` / `webtransport_h3.rs` と接続テストの期待値を確認する。
識別子だけを先行更新して旧形式のメッセージを draft 23 として送らない。

## 完了条件

- 全伝送路で `moqt-23` を提示し、相手が選択したサブプロトコルを検証できること。
- draft 22 の相手を draft 23 として誤受理しないこと。
- draft 23 の制御メッセージ・パラメータ・FETCH のワイヤ対応を完了してから切り替えること。

## 依存・関連

- 先行: [0379](0379-moqt-draft-23-control-message-length-limit.md)
- 先行: [0380](0380-moqt-draft-23-fill-parameters-count.md)
- 先行: [0381](0381-moqt-draft-23-paused-parameter.md)
- 先行: [0382](0382-moqt-draft-23-fetch-ok-request-ok.md)
- 先行: [0384](0384-moqt-draft-23-request-error-details.md)
- 先行: [0386](0386-moqt-draft-23-session-error-classification.md)
- 先行: [0388](0388-moqt-draft-23-publish-fill-fetch.md)
- 先行: [0390](0390-moqt-draft-23-subscribe-tracks-id-parameter.md)
- 先行: [0395](0395-moqt-draft-23-fetch-gone-range.md)
- 先行: [0396](0396-moqt-draft-23-fetch-range-delta.md)
- 先行: [0397](0397-moqt-draft-23-fetch-object-id-delta.md)
- 先行: [0398](0398-moqt-draft-23-fetch-group-track-end.md)
- 先行: [0403](0403-moqt-draft-23-request-and-reset-error-registry.md)

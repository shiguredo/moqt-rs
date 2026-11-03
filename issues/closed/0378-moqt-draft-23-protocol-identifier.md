# プロトコル識別子を moqt-23 に更新する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

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

## 解決方法

- `crates/tokio-moq/src/lib.rs` の `MOQT_PROTOCOL` を `moqt-23` に変更した。QUIC の ALPN、QMux の ALPN、WebTransport の `WT-Available-Protocols` はすべてこの定数を参照するため、全伝送路の提示が一括で切り替わる
- QUIC 直接接続は `s2n-quic` の `application_protocol` で交渉結果を確認し、`moqt-23` 以外なら接続を失敗させる `verify_alpn` を追加した。QUIC は ALPN の選択を必須とするため (RFC 9001 §8.1)、候補外の選択と選択なしは TLS 層でも失敗するが、識別子の一致を接続層でも明示的に確かめる
- QMux は既存の `alpn_protocol()` の検証がそのまま `moqt-23` を要求する。TCP + TLS は ALPN の選択なしでもハンドシェイクが成立するため、この検証が draft-22 の相手を拒否する経路になる
- WebTransport over HTTP/2 は `verify_wt_protocol` が `MOQT_PROTOCOL` と一致する 1 要素だけを受理するようにし、エラーメッセージも定数から組み立てて識別子の二重管理をなくした
- WebTransport over HTTP/3 は `WT-Available-Protocols` に `MOQT_PROTOCOL` を 1 要素で載せる。応答の `WT-Protocol` の検証は h3 層 (shiguredo_http3) が行い、交渉失敗は `WT_ALPN_ERROR` の `SessionClosed` として届いて `TransportError::ProtocolNegotiationFailed` になる既存経路を確認した
- テストとドキュメントの `moqt-22` リテラルを `moqt-23` に更新した。draft-22 参照の全面更新は別 issue の範囲とし、プロトコル識別子の記述だけを直した

## 完了条件の検証

| 完了条件 | 結果 | 検証方法 |
| --- | --- | --- |
| 全伝送路で `moqt-23` を提示し、相手が選択したサブプロトコルを検証できること | 満たす | QUIC は `verify_alpn` と `application_protocol`、QMux は `alpn_protocol`、WebTransport over HTTP/2 は `verify_wt_protocol`、WebTransport over HTTP/3 は h3 層の `WT-Protocol` 検証で確認する |
| draft 22 の相手を draft 23 として誤受理しないこと | 満たす | `moqt-22` だけを広告する QUIC / QMux との実接続テスト、`WT-Protocol` に `moqt-22` を返す WebTransport over HTTP/2 との実接続テスト、`verify_alpn` / `verify_wt_protocol` の単体テストで確認する |
| draft 23 の制御メッセージ・パラメータ・FETCH のワイヤ対応を完了してから切り替えること | 先行 issue の完了を前提とする | 本 issue は識別子だけを変更する。draft 23 のワイヤ対応は 0379〜0403 が担うため、本 issue のコミットはそれらの完了後に行う |

- 検証コマンド: `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` / `RUSTDOCFLAGS="-D warnings" cargo doc -p shiguredo_moqt --no-deps` がすべて成功した
- 追加したテスト: `quic::tests::verify_alpn_accepts_moqt_23`、`quic::tests::verify_alpn_rejects_other_identifiers`、
  `quic::tests::connect_rejects_a_peer_without_alpn_selection`、`quic::tests::connect_rejects_a_draft_22_peer`、
  `qmux::tests::qmux_connect_rejects_a_peer_without_alpn_selection`、`qmux::tests::qmux_connect_rejects_a_draft_22_peer`、
  `webtransport_h2::tests::wt_h2_rejects_a_draft_22_selection`
- 既存テストの更新: `verify_wt_protocol_accepts_moqt_22` を `verify_wt_protocol_accepts_moqt_23` に改名し、
  `build_connect_headers_offers_moqt_protocol` / `verify_wt_protocol_ignores_parameters` /
  `verify_wt_protocol_rejects_invalid_selection` のリテラルを `moqt-23` に更新した

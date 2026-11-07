# 0-RTT 再開時の前回 WebTransport SETTINGS を現接続で使わないことを固定する

- Created: 2026-11-03
- Completed: 2026-11-08
- Branch: develop
- Polished: 2026-11-08

## 目的

[draft-ietf-webtrans-http3-17 §3.2](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従する。draft-17 を選択した接続では、前回接続から保持した WebTransport SETTINGS を破棄し、現接続で受信した SETTINGS だけを使う。

本 issue は 0407 の依存更新後に、adapter が前回接続の SETTINGS を保持・注入せず、0-RTT も使わないことを doc と Sans I/O テストで固定する。production code の機能変更は行わない。

## 現状

0407 で `shiguredo_http3` を `2026.1.0-canary.14` に更新する。canary.14 の `Connection::set_previous_wt_settings` は draft-14 / 15 / 16 の 0-RTT 用であり、draft-17 を選択した接続では前回値を使わないと rustdoc に明記している。draft-14 / 15 では従来どおり前回値からの減少を `H3_SETTINGS_ERROR` にする。

`crates/tokio-moq/src/webtransport_h3.rs` は `set_previous_wt_settings` を呼ばず、`ClientConnectionState` も前回接続の SETTINGS を保持しない。TLS のセッションストアも 0-RTT の early data も設定せず、`ClientTlsConfig::new` と `Client::connect` だけを使う。CONNECT の前には `wait_for_peer_settings` が現接続の SETTINGS を待つ。

これらの不変条件を固定するテストは無い。0407 の依存更新だけでは、将来 adapter が前回値の注入や 0-RTT を追加しても検出できない。

## 設計方針

production code は変更しない。0407 の依存更新と `wait_for_peer_settings` の版確認で、draft-17 の §3.2 は満たされる。

`crates/tokio-moq/src/webtransport_h3.rs` の `WtClient::connect` の doc に、本 adapter は前回接続の WebTransport SETTINGS を保持・注入せず、0-RTT も使わないこと、SETTINGS は現接続で受信した値だけを使うことを明記する。0407 が更新するモジュール doc と `wait_for_peer_settings` の説明とは重ならない範囲にする。
`crates/tokio-moq/Cargo.toml`、`crates/moq-pub/Cargo.toml`、`crates/moq-sub/Cargo.toml`、`Cargo.lock` は変更しない (0407 が所有する)。

`crates/tokio-moq/src/webtransport_h3.rs` の `#[cfg(test)] mod tests` に、依存の版ゲートを固定する Sans I/O テストを追加する。モックは使わず、`ClientConnection` と `peer_control_stream_bytes` を使う。

- draft-17 のローカル設定で `set_previous_wt_settings` に大きい初期値を注入し、その後で減少・省略した draft-17 の peer SETTINGS を流しても `H3_SETTINGS_ERROR` にならず、`negotiated_wt_draft_version()` が `Draft17` になること。
- 対照として、draft-15 のローカル設定で同じ前回値を注入し、減少した draft-15 の peer SETTINGS を流すと `H3_SETTINGS_ERROR` になること。draft-17 の除外が版ゲートによるものであることを示す。
- 前回値を注入しても `wait_for_peer_settings` は現接続の SETTINGS 到着前に完了せず、現接続の SETTINGS を流した後に完了すること。

0-RTT の利用経路は追加しない。`ClientTlsConfig` にセッションストアを設定せず、`Connect::with_early_peer_params` と `Client::connect_early` を使わない。この非目標を doc とテストで固定する。

## 完了条件

- draft-17 のローカル設定で前回値の減少・省略が `H3_SETTINGS_ERROR` にならず、`Draft17` を選択したままであること。
- draft-15 のローカル設定では同じ前回値と減少値が `H3_SETTINGS_ERROR` になること。
- 前回値を注入しても、現接続の SETTINGS 到着前に `wait_for_peer_settings` が完了しないこと。現接続の SETTINGS を流した後に完了すること。
- `crates/tokio-moq/src/webtransport_h3.rs` の production code に `set_previous_wt_settings` / `connect_early` / `with_session_store` / `early_peer_params` を使う経路が無いこと。diff で確認する。
- `WtClient::connect` の doc が、前回接続の SETTINGS を保持・注入せず 0-RTT も使わないことを説明していること。

## 差分の確認範囲

- `SETTINGS_WT_ENABLED` の codepoint と版選択は 0407 で追跡する。本 issue では `Cargo.*` と版選択の実装を変更しない。
- WT 以外の HTTP/3 SETTINGS (QPACK など) の 0-RTT 整合性規則は変えない。本 issue は WebTransport SETTINGS だけを扱う。
- §4.6 の「クライアントは CONNECT の前に現接続の SETTINGS を受信する」MUST は、`wait_for_peer_settings` の既存挙動で満たす。テストでその待機を固定する。
- §9.2 の codepoint と初期値の登録は 0407 の対象である。

## 依存・関連

- 先行: [0407](closed/0407-wt-h3-draft-17-settings-version-negotiation.md)

## 解決方法

`WtClient::connect` の doc に、前回接続の WebTransport SETTINGS を保持・注入せず 0-RTT も使わないこと、SETTINGS は現在の接続で受信した値だけを使うことを明記した (draft-ietf-webtrans-http3-17 §3.2)。

`crates/tokio-moq/src/webtransport_h3.rs` のテストに、次の 4 ケースを追加した。production code の機能変更はない。

- draft-17: 前回値より小さい現在値が `H3_SETTINGS_ERROR` にならず、現在値がそのまま反映される
- ローカルが新旧両方を広告する場合、前回値が draft-17 でも旧 codepoint だけの peer とは Draft15 を選ぶ
- draft-15 の対照: 前回値より小さい現在値は `H3_SETTINGS_ERROR` (`0x109`) になる
- 前回値だけでは `wait_for_peer_settings` が完了せず、現在の SETTINGS の到着で完了する

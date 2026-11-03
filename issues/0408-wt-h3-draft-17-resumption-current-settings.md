# 0-RTT 再開時の WebTransport SETTINGS を現接続から評価する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-wt-h3-draft-17-resumption-current-settings
- Polished: 2026-11-03

## 目的

[draft-ietf-webtrans-http3-17 §3.2、§9.2](../refs/webtrans/draft-ietf-webtrans-http3-17.txt) の変更に追従する。

## 現状

固定中の `shiguredo_http3 2026.1.0-canary.13` の `src/connection/wt_session.rs` は `set_previous_wt_settings` を公開し、`src/connection/mod.rs` の SETTINGS 処理が前接続から初期フロー制御値が減った場合に H3_SETTINGS_ERROR を返す。
`crates/tokio-moq/src/webtransport_h3.rs` は現在この API を呼んでいないため、現行 adapter でこのエラーを再現したという指摘ではない。

## 設計方針

draft 17 で撤回された前接続値との減少検証を行わず、§9.2 の全 WebTransport SETTINGS を現接続の値で判断する依存版へ追従する。
WT 以外の HTTP/3 SETTINGS の 0-RTT 整合性規則は変えない。
adapter が将来キャッシュ値を注入して旧条件を復活させないよう、設定・接続の責務を明記する。

## 完了条件

- 再開接続で WT の設定値が減少・省略されても、それだけで H3_SETTINGS_ERROR にならないこと。
- 現接続の設定を受信する前にキャッシュ値を用いて WT の CONNECT や配送を開始しないこと。
- 依存側の仕様対応を確認し、adapter に新たな 0-RTT 利用経路を追加せずに追従できること。

## 依存・関連

- 関連: [0407](0407-wt-h3-draft-17-settings-version-negotiation.md)

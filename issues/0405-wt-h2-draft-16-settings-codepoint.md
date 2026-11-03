# WebTransport over HTTP/2 の SETTINGS 識別子を draft 16 に更新する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-wt-h2-draft-16-settings-codepoint
- Polished: 2026-11-03

## 目的

[draft-ietf-webtrans-http2-16 §3.1、§9.1、§11.2](../refs/webtrans/draft-ietf-webtrans-http2-16.txt) の変更に追従する。

## 現状

`crates/tokio-moq/src/webtransport_h2.rs` の `build_limits` / `wait_for_peer_settings` は `shiguredo_http2` の Settings API に依存する。
固定中の `2026.1.0-canary.16` の `src/settings.rs` の `Setting::from_wire` / `as_wire` は SETTINGS_WT_ENABLED を `0x2b60` として処理する。
draft 16 は `0x2b67` に変更しており、現状では対応を認識できない。

## 設計方針

新しい識別子に対応する依存版を取り込み、adapter の SETTINGS 生成・受信判定と結合テストを更新する。
旧 ID を同じ版の宣言と誤認しない。
旧版との共存を選ぶ場合は明示的な版の識別を持たせる。
新しい ID の対応だけで draft 16 のフロー制御規則まで対応済みとはしない。

## 完了条件

- `0x2b67=1` と ENABLE_CONNECT_PROTOCOL の組で CONNECT を開始できること。
- `0x2b60` だけの相手、および `0x2b67=0` または新 ID を省略した相手は非対応として CONNECT を開始しないこと。
- `0x2b67` の値が 1 を超える場合は、§3.1 に従って HTTP/2 connection error の `PROTOCOL_ERROR` とすること。
- `crates/tokio-moq/Cargo.toml` / `Cargo.lock`、コメント、H2 の交渉テストが同じ版を示すこと。

## 差分の確認範囲

- §3.4 / §6 の stream error から session error への表現変更は、既存の DriverState が CONNECT stream を reset する扱いで対応している。
- §6.4 の session data / stream data / stream count 上限の明記は、依存の WtSession と adapter の設定・エラー伝搬で扱っている。新しく厳しくなった同値拒否は 0406 で追跡する。
- §4.3.2 の初期 stream data と session credit の説明、stream ID の図表、例の Origin / SETTINGS、参照節と IANA の追加は確認済み。初期値を SETTINGS で広告する現行 client 方針を維持する。
- Structured Fields の規範参照は RFC 8941 から RFC 9651 に変わる。依存の WtInit パーサーは旧参照だが、現行の client adapter は CONNECT 要求の WebTransport-Init を解析しない。依存更新時に参照と適用範囲を確認し、このリポジトリへ server 用パーサーを追加しない。

## 依存・関連

- 関連: [0406](0406-wt-h2-draft-16-flow-control-strict-increase.md)

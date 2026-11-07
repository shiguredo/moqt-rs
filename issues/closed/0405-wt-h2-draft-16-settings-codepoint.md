# WebTransport over HTTP/2 の SETTINGS 識別子を draft 16 に更新する

- Created: 2026-11-03
- Completed: 2026-11-08
- Branch: develop
- Polished: 2026-11-08

## 目的

[draft-ietf-webtrans-http2-16 §3.1、§9.1、§11.2](../refs/webtrans/draft-ietf-webtrans-http2-16.txt) の変更に追従する。

## 現状

`crates/tokio-moq/src/webtransport_h2.rs` の `build_limits` / `wait_for_peer_settings` は `shiguredo_http2` の Settings API に依存する。
固定中の `2026.1.0-canary.17` の `src/settings.rs` の `Setting::from_wire` / `as_wire` は SETTINGS_WT_ENABLED を `0x2b67` として処理し、旧 draft-15 の `0x2b60` は未知の SETTINGS として無視する。
adapter は `build_limits` で `wt_enabled(true)` を広告し、`wait_for_peer_settings` で peer の `wt_enabled()` を確認する。`0x2b60` を依存が未知の SETTINGS として無視し、adapter が `wt_enabled()` 偽として CONNECT を開始しないため、旧 ID だけの相手は非対応になる。
値が 1 を超える SETTINGS は依存が `PROTOCOL_ERROR` の connection error にするが、adapter はそのエラーを返すだけで GOAWAY を送らない。
`0x2b60` だけの相手、`0x2b67=0`、`0x2b67` を 1 より大きくした相手を実 peer で検証するテストは無い。

## 設計方針

依存の canary.17 が `0x2b67` の受理、`0x2b60` の無視、1 を超える値の connection error を実装している。adapter 側で codepoint の判定を再実装せず、その挙動を実 peer との結合テストで固定する。
SETTINGS の connection error は GOAWAY で peer に伝える。エラーを返すだけでは HTTP/2 の connection error の伝達にならないため、受信経路で依存が返した connection error のコードを GOAWAY に載せる。
旧 ID を同じ版の宣言と誤認しない。
旧版との共存は選ばない。将来共存する場合は明示的な版の識別を持たせる。
新しい ID の対応だけで draft 16 のフロー制御規則まで対応済みとはしない。

## 完了条件

- `0x2b67=1` と ENABLE_CONNECT_PROTOCOL を広告する実 HTTP/2 peer と CONNECT を開始できること。
- `0x2b60=1` だけの相手、`0x2b67=0` の相手、新 ID を省略した相手には CONNECT を開始せず、`InvalidState` で失敗すること。3 変種とも ENABLE_CONNECT_PROTOCOL=1 を併せて広告し、失敗の理由が旧 ID の無視・値 0・省略であることを切り分けること。
- `0x2b67` の値が 1 を超える場合、client が `PROTOCOL_ERROR` の GOAWAY を送り、`TransportError::Http2` で connection error を返すこと。
- `crates/tokio-moq/Cargo.toml` / `Cargo.lock` が `2026.1.0-canary.17` を指し、`crates/tokio-moq/src/webtransport_h2.rs` のコメントとテストが draft-16 の `0x2b67` を参照すること。

## 差分の確認範囲

- §3.4 / §6 の stream error から session error への表現変更は、既存の DriverState が CONNECT stream を reset する扱いで対応している。
- §6.4 の session data / stream data / stream count 上限の明記は、依存の WtSession と adapter の設定・エラー伝搬で扱っている。新しく厳しくなった同値拒否は 0406 で追跡する。
- §4.3.2 の初期 stream data と session credit の説明、stream ID の図表、例の Origin / SETTINGS、参照節と IANA の追加は確認済み。初期値を SETTINGS で広告する現行 client 方針を維持する。
- §4.3.1 の ACK 済み SETTINGS のスナップショットは本 issue の対象外とする。現行実装は CONNECT 応答後に `io.remote_settings()` を読むため、CONNECT 後に更新された SETTINGS をセッション初期値に使い得る。この MUST は SETTINGS の codepoint 追従とは別の課題として扱う。
- Structured Fields の規範参照は RFC 8941 から RFC 9651 に変わる。canary.17 の WtInit パーサーは RFC 9651 を参照している。現行の client adapter は CONNECT 要求の WebTransport-Init を解析しないため、このリポジトリへ server 用パーサーを追加しない。

## 依存・関連

- 関連: [0406](0406-wt-h2-draft-16-flow-control-strict-increase.md)

## 解決方法

`crates/tokio-moq/src/webtransport_h2.rs` で、接続エラーを検知したら依存のエラーコードを GOAWAY に載せてから失敗するようにした。RFC 9113 §5.4.1 の connection error の伝達と、draft-16 §3.1 の `0x2b67 > 1` の PROTOCOL_ERROR を満たす。

自己署名証明書を使う実 TLS のテストサーバーに、任意の SETTINGS を生フレームで送る経路を追加した。クライアントの SETTINGS が `0x2b67=1` を含み `0x2b60` を含まないことも wire で検証する。

追加したテスト:

- `wt_h2_accepts_draft_16_settings_codepoint`: `0x2b67=1` と ENABLE_CONNECT_PROTOCOL で CONNECT を開始する
- `wt_h2_rejects_old_settings_codepoint`: `0x2b60=1` だけの peer とは CONNECT を開始しない
- `wt_h2_rejects_zero_wt_enabled_value`: `0x2b67=0` の peer とは CONNECT を開始しない
- `wt_h2_rejects_non_boolean_wt_enabled_value`: `0x2b67=2` で PROTOCOL_ERROR の GOAWAY を送る

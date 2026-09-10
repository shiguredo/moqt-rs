# Authorization Token キャッシュの §8.9 / §9.1.4 MUST を満たす

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-auth-token-cache-musts

## 目的

draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression) と §9.1.4 (AUTHORIZATION TOKEN) の MUST を満たす。期限切れ token の扱いと、peer の `MAX_AUTH_TOKEN_CACHE_SIZE` に基づくローカル alias の purge を実装または明示的に責務化する。

## 現状

`src/session/auth_token_cache.rs` の `AuthTokenCache` は Token Type / Value のみ保持し、期限・失効時刻を一切持たない。そのため USE_ALIAS 時に期限切れを判定できず、`SESSION_EXPIRED_AUTH_TOKEN` / `REQUEST_EXPIRED_AUTH_TOKEN` / `STREAM_EXPIRED_AUTH_TOKEN` を送出する経路が無い (`src/error.rs` に定義のみ)。

また `src/session/core.rs` はローカル SETUP の REGISTER alias を使い捨て cache に登録後すぐ破棄し、`Session` に保持しない。peer の `MAX_AUTH_TOKEN_CACHE_SIZE` を読む公開アクセサも無い。そのため、peer の cache サイズに収まらなかったローカル alias を把握できず purge もできない。

根拠:

- §8.9: "If a receiver detects that an authorization token has expired, it MUST retain the registered Alias until it is deleted by the sender ... Any message that references an expired token with Alias Type USE_ALIAS fails with EXPIRED_AUTH_TOKEN."
- §9.1.4: "the sender MUST handle registration failures of this kind by purging any Token Aliases that failed to register based on the peer's MAX_AUTH_TOKEN_CACHE_SIZE option in SETUP (or the default value of 0)."

期限切れ判定は Token Type 固有 (Type 0 は out-of-band 交渉) であり、ライブラリ単独で判定できない可能性がある。その場合でもアプリが期限・エラー状態を登録して `EXPIRED_AUTH_TOKEN` を返せる API が必要。

## 設計方針

次のいずれかを設計判断する。

- `AuthTokenCache` に期限・エラー状態を持たせ、USE_ALIAS 時に期限切れを判定して `EXPIRED_AUTH_TOKEN` を返す
- アプリ責務と割り切り、`AuthTokenCache` と `Session` の doc に「期限切れ検出はアプリ責務」と明記し、`EXPIRED_AUTH_TOKEN` の使い方と関連 API を整理する

ローカル alias の purge は、ローカル alias 登録集合を `Session` に保持し、peer SETUP 受信時に peer の cache サイズで再評価して超過分を `UseValue` へフォールバックする。最低限 peer の `max_auth_token_cache_size` を公開する。

## 完了条件

- §8.9 の期限切れ token の扱いが実装または明示的に責務化されていること
- §9.1.4 の purge が実装または明示的に責務化されていること
- `AuthTokenCache` の役割 (保持する alias 空間) が doc と一致すること
- 対応テストまたは doc の記載が追加されていること

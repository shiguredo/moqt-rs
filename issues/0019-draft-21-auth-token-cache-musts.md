# Authorization Token キャッシュの §8.9 / §9.1.4 MUST を満たす

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-auth-token-cache-musts
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression) と §9.1.4 (AUTHORIZATION TOKEN) の MUST を満たす。期限切れ token の扱いをアプリ責務として明示し、peer の `MAX_AUTH_TOKEN_CACHE_SIZE` に基づくローカル alias の purge をアプリが実行できる API を追加する。

## 現状

- `src/session/auth_token_cache.rs` の `AuthTokenCache` は alias ごとに Token Type / Token Value のみ保持し、期限・エラー状態を一切持たない。USE_ALIAS 解決は登録の有無だけを見る (`resolve`) ため、`Session` が USE_ALIAS 処理時に期限切れを自動判定する経路が無い。
- `SESSION_EXPIRED_AUTH_TOKEN` / `REQUEST_EXPIRED_AUTH_TOKEN` / `STREAM_EXPIRED_AUTH_TOKEN` は `src/error.rs` に定義済みで、アプリが既存 API (`close` / `send_request_error` / `reset_outgoing_data_stream_with_code`) に渡せば送出できる。欠けているのは自動判定経路と、どの文脈でどのコードを使うかの doc である。
- 自側 SETUP は `SetupState.local` に保持される (`Session::new` で重複検証に使う cache が使い捨てなだけ)。peer SETUP も `SetupState.peer` に保持される。しかし peer の `MAX_AUTH_TOKEN_CACHE_SIZE` をアプリへ渡す公開アクセサが無く、アプリが purge 対象 alias を判断できない。
- peer SETUP の REGISTER が自側 MAX を超えた場合に session を閉じず USE_VALUE 扱いにする §9.1.4 の受信側 MUST は、`register_setup_auth_tokens` が `Ok(false)` を無視する形で既に実装済みである (本 issue の対象外)。
- `AuthTokenCache` の struct doc は「自側が登録した alias と相手が登録した alias の 2 つを保持する」と書くが、1 インスタンスが保持するのは片方向の alias 空間であり、`Session` が公開するのは `peer_token_cache` の 1 つだけである。

根拠:

- §8.9: "If a receiver detects that an authorization token has expired, it MUST retain the registered Alias until it is deleted by the sender ... Any message that references an expired token with Alias Type USE_ALIAS fails with EXPIRED_AUTH_TOKEN."
- §9.1.4: "the sender MUST handle registration failures of this kind by purging any Token Aliases that failed to register based on the peer's MAX_AUTH_TOKEN_CACHE_SIZE option in SETUP (or the default value of 0)."

期限切れ判定は Token Type 固有 (Type 0 は out-of-band 交渉) であり、ライブラリ単独で判定できない。

## 設計方針

- 期限切れ検出はアプリ責務と割り切る。`AuthTokenCache` に期限・エラー状態を登録する新 API は追加しない (Token Type の解釈がライブラリに漏れ込むため)。`AuthTokenCache` と `Session` の doc に次を明記する。
  - alias は期限切れ後も DELETE まで保持する (§8.9 MUST)。現行実装の挙動がそのまま該当する
  - アプリが期限切れを検出したときの応答: 受信 request 文脈は `send_request_error(request_id, REQUEST_EXPIRED_AUTH_TOKEN, ...)`、SETUP などセッション文脈は `close(SESSION_EXPIRED_AUTH_TOKEN, ...)`、data stream 文脈は `reset_outgoing_data_stream_with_code(stream_id, STREAM_EXPIRED_AUTH_TOKEN)`
  - §9.20.3 の MUST (request が他の理由で失敗しても REGISTER は cache に登録する) に従い、期限切れで拒否する場合も alias 登録は維持する
- §9.1.4 の sender MUST をアプリが実行できるよう、公開アクセサ `Session::peer_max_auth_token_cache_size() -> u64` を追加する。peer SETUP 未受信時は仕様どおりデフォルト 0 を返す。アプリは自側 SETUP の REGISTER alias と peer max を突き合わせ、収まらない alias を使うメッセージを `UseValue` へフォールバックする。ライブラリはメッセージを組み立てない (Sans I/O) ため、purge の実行主体はアプリとする。
  - doc にサイズ計算 (§9.1.3: 16 バイト + Token Value 長) と `AuthTokenCache::entry_size` との対応を書く
- `AuthTokenCache` の struct doc を実態 (1 インスタンス 1 alias 空間。`Session` は peer 側を `peer_auth_token_cache()` で公開) に合わせる。`peer_auth_token_cache()` の doc には、`max_size()` が自側 `MAX_AUTH_TOKEN_CACHE_SIZE` であり peer の宣言値ではないことを明記し、peer max は新アクセサで取れると書き分ける。
- 公開 API の追加のため、`CHANGES.md` の `## develop` に `[ADD]` として記載する。
- 対象外: §9.1.4 の受信側 MUST (peer SETUP REGISTER overflow を USE_VALUE 扱いにする) は実装済みのため変更しない。

## 完了条件

- `Session::peer_max_auth_token_cache_size()` が追加され、peer SETUP 受信前は 0、受信後は peer の `MAX_AUTH_TOKEN_CACHE_SIZE` (未指定は 0) を返すこと
- `AuthTokenCache` と `Session` の doc に、期限切れ検出がアプリ責務であること、alias を DELETE まで保持する §8.9 MUST、3 文脈の `EXPIRED_AUTH_TOKEN` コードの使い分けが記載されていること
- `peer_auth_token_cache()` の `max_size()` が自側 MAX であることと、peer max が新アクセサで取得できることが doc で区別されていること
- `AuthTokenCache` の struct doc が 1 インスタンス 1 alias 空間の実態と一致すること
- 新アクセサのテスト (peer SETUP 未受信時の 0 / peer が指定した値) が `tests/test_session/` に追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[ADD]` として記載されていること

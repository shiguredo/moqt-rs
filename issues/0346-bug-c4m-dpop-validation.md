# C4M の DPoP / トークン束縛の検証を仕様に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-c4m-dpop-validation
- Polished: 2026-10-21

## 目的

draft-ietf-moq-c4m-01 の DPoP 検証に次の不備がある。認可とリプレイ防御の正しさに影響するため修正する。

1. 接続確立 (CLIENT_SETUP / SERVER_SETUP) 用の `resource` (`moqt://<relay-endpoint>`) を `ResourceInconsistent` で拒否する。
2. §3.1.3 が定義する namespace 操作の `resource` (`moqt://<relay-endpoint>?tns=<namespace>`) を、actx に `tn` がある場合に拒否する。
3. `DpopReplayCache` の保持期間が現在の proof の `catdpop` ウィンドウで決まり、同一キャッシュを異なるウィンドウのトークンで使うと jti を再受理し得る。
4. `cnf` の confirmation key 3 を常に JWK サムプリントとして扱い、RFC 8747 が 3 に割り当てる `kid` と区別できない。

## 現状

### SETUP 用 resource の拒否

- `src/c4m/dpop.rs` の `AuthorizationContext::verify_resource_consistency` は、`resource` がある場合にクエリの `tns` と actx の `tns` の一致を要求し、actx に `tn` がある場合はクエリの `tn` も要求する。
- draft-ietf-moq-c4m-01 §3.1.3 は接続確立の resource を `moqt://<relay-endpoint>` (クエリ無し) と定義する。この形式にはクエリの `tns` が無いため、`verify_resource_consistency` は `ResourceInconsistent` を返す。
- `tests/test_c4m/dpop.rs` の `resource_consistency_errors` は `resource = "moqt://relay.example.com"` に対して `ResourceInconsistent` を期待しており、拒否を固定している。
- `DpopProof::verify_authorization_context` は `verify_resource_consistency` の後に `verify_target(namespace, track_name)` を呼ぶ。SETUP には track が無いため、SETUP の proof に対して `namespace` / `track_name` に何を渡すか、`verify_target` をどう扱うかが未定義である。

### namespace 操作の resource の拒否

- draft-ietf-moq-c4m-01 §3.1.3 は namespace 操作の resource を `moqt://<relay-endpoint>?tns=<namespace>` (`tn` 無し) と定義する。
- 現行の `verify_resource_consistency` は actx に `tn` がある場合にクエリの `tn` も要求する。`verify_target` が actx の `tn` を必須にするため、namespace 操作の proof は `tn` を持ち、`resource = "moqt://relay.example.com?tns=example.2ecom"` でも `ResourceInconsistent` になる。
- §3.1.2 の「resource は `tns` / `tn` と一致する MUST」と §3.1.3 の namespace 形式 (`tn` 無し) は字面上矛盾する。

### リプレイキャッシュの保持期間

- `src/c4m/dpop.rs` の `DpopReplayCache::check_and_record` は `self.entries.retain(|(_, recorded_at)| reference_time_seconds - recorded_at <= window_seconds)` で、現在の proof の `window_seconds` を保持期間として古いエントリを破棄する。
- 同一キャッシュをウィンドウの異なるトークンで共有すると、小さいウィンドウの proof が大きいウィンドウの proof より先に到達した場合に、まだ大きいウィンドウ内にある jti が破棄され、後から大きいウィンドウの proof で再送すると `Replayed` にならず受理される。
- 再現例: `iat = 100`、`window = 60` の jti X を記録する。次に `iat = 130`、`window = 10` の proof を同じキャッシュで処理すると、X は `130 - 100 > 10` で破棄される。その直後に `iat = 100`、`window = 300` の X を再送すると、`135 - 100 <= 300` であり、X が記録に無いため受理される。

### cnf の confirmation key 3

- `src/c4m/cat.rs` の `Confirmation::decode` は key 3 を `c4m_draft_jwk_thumbprint` に格納し、`Confirmation::jkt` は 323 が無い場合に key 3 を返す。`DpopProof::verify_key_binding` はその値を proof の JWK サムプリントと比較する。
- RFC 8747 §3.4 / §7.2 は CWT Confirmation Methods の key 3 を `kid` (Key Identifier) と定義する。
- draft-ietf-moq-c4m-01 §3.1.1 と付録 A.4 は key 3 を `jkt` (JWK Thumbprint) として使う。付録 A.4 の `payload_cbor_hex` は `...08a1035820...` であり、`cnf = {3: bstr}` である。
- 現行実装は key 3 を常にサムプリントとして扱うため、`cnf: {3: kid}` の kid をサムプリントとして比較し、kid ベースの PoP と C4M draft 形式を区別できない。

## 設計方針

### action に応じた resource 検証

- `AuthorizationContext::verify_resource_consistency` のシグネチャを `pub fn verify_resource_consistency(&self, action: MoqtAction) -> Result<(), DpopError>` に変更する。
- action を次の 3 種類に分ける。
  - SETUP: `MoqtAction::ClientSetup` / `MoqtAction::ServerSetup`。
  - namespace 操作: `MoqtAction::PublishNamespace` / `MoqtAction::SubscribeNamespace`。
  - track 操作: `MoqtAction::Subscribe` / `MoqtAction::RequestUpdate` / `MoqtAction::Publish` / `MoqtAction::Fetch` / `MoqtAction::TrackStatus`。
- SETUP では、`resource` がある場合に `moqt://<authority>` の形式だけを検証する。
  - `moqt://` (小文字) で始まること。
  - authority が空でないこと。
  - クエリ (`?`)、フラグメント (`#`)、パス (`/`)、userinfo (`@`)、空白を含まないこと。
  - クエリの `tns` / `tn` と actx の `tns` / `tn` は比較しない (§3.1.3 の接続確立用 resource にクエリが無いため)。
  - `resource` が無い場合は受理する (optional)。
- namespace 操作では、`resource` がある場合にクエリの `tns` と actx の `tns` の一致を要求する。actx に `tn` があり、クエリに `tn` がある場合は一致を要求し、クエリに `tn` が無い場合は比較を省略する (§3.1.3 の namespace 形式に `tn` が無いため)。
- track 操作では、`resource` がある場合にクエリの `tns` と actx の `tns`、クエリの `tn` と actx の `tn` の一致を要求する。actx に `tn` が無い場合はクエリの `tn` の検証を省略し、`verify_target` が `MissingTrackName` で拒否する。
- クエリパラメータの重複時の扱いは現行どおり (最後の値を使う)。重複の厳密な検証は 0356 のモデル独立化で扱う。
- `DpopProof::verify_authorization_context` は、SETUP では `verify_target` を呼ばず、actx の `track_name` の存在だけを要求する (§3.1.2 の `tn` 必須)。SETUP 以外では現行どおり `verify_target` を呼ぶ。
- `DpopVerification` の `namespace` / `track_name` は SETUP では使わないことを doc に明記する。呼び出し側は SETUP では任意の値 (空の namespace など) を渡せる。
- §3.1.2 の「resource は `tns` / `tn` と一致する MUST」と §3.1.3 の接続確立 / namespace 形式は字面上矛盾する。本 issue では §3.1.3 の action 別形式を優先し、resource に無いパラメータは比較しない。
- endpoint の値 (host / port) と接続先の一致は検証しない。authority の完全な RFC 3986 検証も行わない。resource の値は proof のクレームとして保持されるため、接続先と比較する必要がある利用側は `proof.claims().authorization_context.resource` を自分で確認する。

### リプレイキャッシュの最大ウィンドウ

- `DpopReplayCache` に `max_window_seconds` を持たせる。
- `DpopReplayCache::new()` の既定値を 300 秒 (draft-ietf-moq-c4m-01 §3.1.1 の `catdpop` の例と同じ) とし、`DpopReplayCache::with_max_window(max_window_seconds: f64) -> Result<Self, DpopError>` で変更できるようにする。`max_window_seconds` は有限かつ 0 より大きくなければならない。`Default` は `new()` と同じ既定値にする。
- `check_and_record` は、現在の proof の `window_seconds` ではなく `max_window_seconds` でエントリを purge する。
- `window_seconds > max_window_seconds` の proof は、キャッシュの保持期間より長いウィンドウを要求するため、新しいエラー `DpopError::ReplayWindowTooLarge` で拒否する (fail-closed)。利用側は、そのトークンを受け入れるなら `with_max_window` でより大きい値を設定したキャッシュを使う。
- `DpopReplayCache` の doc に、1 つの主体 (セッション / トークン) ごとに保持し、`max_window_seconds` はその主体で受け入れる `catdpop` ウィンドウの最大値以上にすることを明記する。
- `DpopError` に `ReplayWindowTooLarge` と、`with_max_window` の不正値用の `InvalidReplayWindow` を追加し、`Display` を更新する。

### cnf の key 3 の解釈

- `src/c4m/cat.rs` に `ConfirmationKey3Interpretation` を追加する。
  - `IanaKid` (既定): RFC 8747 に従い、key 3 を `kid`、key 323 を `jkt` として扱う。
  - `C4mDraftJkt`: draft-ietf-moq-c4m-01 §3.1.1 / 付録 A.4 に従い、key 3 を `jkt` として扱う。
- `Confirmation` は key 3 の生値を保持する。既存の `c4m_draft_jwk_thumbprint` フィールドを `confirmation_key_3` に改名する。
- `Confirmation::jkt` は既定で 323 だけを返し、`Confirmation::jkt_with(interpretation)` は `C4mDraftJkt` の場合だけ key 3 にフォールバックする。`Confirmation::kid(interpretation)` は `IanaKid` の場合に key 3 を返す。
- `DpopProof::verify_key_binding` は `ConfirmationKey3Interpretation` を受け取り、`jkt_with(interpretation)` を使う。`IanaKid` で key 3 しか無い場合は `MissingThumbprint` を返す。kid ベースの PoP の鍵解決 / 検証は本ライブラリでは行わない。利用側は `Confirmation::kid` で kid を取り出して鍵を解決する。
- `DpopVerification` に `confirmation_key3: ConfirmationKey3Interpretation` を追加し、`verify_against_token` はそれを使う。
- 既存の `verify_against_cat_token` は既定の `IanaKid` を使う。`C4mDraftJkt` を指定できる `verify_against_cat_token_with` を追加する。
- `CatTokenBuilder::c4m_draft_jwk_thumbprint` は key 3 を設定するメソッドとして残し、doc に IANA では kid、C4M draft では jkt と明記する。
- 既定は `IanaKid` とし、C4M draft-01 の付録 A.4 のベクタを通すテストは `C4mDraftJkt` を明示する。
- `docs/c4m.md` と `skills/shiguredo-moqt/SKILL.md` に、既定と互換オプション、および A.4 が互換オプションを必要とすることを明記する。

## 変更対象

- `src/c4m/dpop.rs`
- `src/c4m/cat.rs`
- `tests/test_c4m/dpop.rs`
- `tests/test_c4m/cat.rs`
- `pbt/tests/prop_c4m/dpop.rs`
- `pbt/tests/prop_c4m/cat.rs`
- `fuzz/fuzz_targets/fuzz_verify_c4m_dpop_context.rs`
- `fuzz/fuzz_targets/fuzz_c4m_authorization_context.rs`
- `docs/c4m.md`
- `skills/shiguredo-moqt/SKILL.md`

## 完了条件

### resource 検証

- `verify_resource_consistency(MoqtAction::ClientSetup)` と `verify_resource_consistency(MoqtAction::ServerSetup)` が、`resource = Some("moqt://relay.example.com")` を受理すること。
- SETUP の `resource` が `moqt://` で始まらない、authority が空、クエリ / フラグメント / パス / userinfo / 空白を含む場合は `ResourceInconsistent` で拒否すること。
- `verify_authorization_context` が SETUP で `verify_target` を呼ばず、actx の `track_name` が無い場合は `MissingTrackName` で拒否すること。
- namespace 操作で `verify_authorization_context` が `resource = "moqt://relay.example.com?tns=example.2ecom"` を受理し、`tns` の不一致、`tn` が存在する場合の不一致を `ResourceInconsistent` で拒否すること。
- track 操作で `resource` があり、actx に `tn` がある場合に `tns` と `tn` の両方の一致を要求し、`tn` が無い / 不一致の場合は `ResourceInconsistent` で拒否すること。actx に `tn` が無い場合は `verify_target` が `MissingTrackName` で拒否すること。
- SETUP 以外では `verify_target` が引き続き `tns` / `tn` を target と比較すること。
- `tests/test_c4m/dpop.rs` の `resource_consistency_errors` を新しい意味に更新し、上記の positive / negative テストを追加すること。

### リプレイキャッシュ

- `max_window_seconds = 300` の同一キャッシュで、`iat = 100` / `window = 60` の jti X を記録後、`iat = 130` / `window = 10` の proof を記録しても X が保持され、`iat = 135` / `window = 300` の X が `Replayed` になること。
- `max_window_seconds` より大きい `window_seconds` の proof が `ReplayWindowTooLarge` で拒否されること。
- `with_max_window` に非有限または 0 以下の値を渡すと `InvalidReplayWindow` になること。
- `tests/test_c4m/dpop.rs` の `replay_cache_records_and_prunes` を新しい意味に更新し、上記の regression テストを追加すること。
- `pbt/tests/prop_c4m/dpop.rs` の `ReplayModel` が `max_window_seconds` と `ReplayWindowTooLarge` に追随し、`cargo test -p pbt` が通ること。

### cnf の解釈

- 既定の `ConfirmationKey3Interpretation::IanaKid` で、`cnf: {323: jkt}` が鍵バインディングを満たし、`cnf: {3: kid}` のみの場合は `MissingThumbprint` になり、`Confirmation::kid(IanaKid)` で kid を取得できること。
- `ConfirmationKey3Interpretation::C4mDraftJkt` で、`cnf: {3: jkt}` のみの場合に鍵バインディングを満たし、C4M draft A.4 のベクタが通ること。
- `cnf` に 323 と 3 の両方がある場合、323 を優先し、3 は解釈に応じて kid / jkt として扱うこと。
- `tests/test_c4m/cat.rs` の `dpop_vectors_decode` と `confirmation_jkt_prefers_iana_key`、`tests/test_c4m/dpop.rs` の `key_binding_prefers_the_iana_thumbprint` を新しい意味に更新し、上記の positive / negative テストを追加すること。
- `Confirmation` のフィールド改名と `verify_key_binding` の引数追加に伴い、`tests/test_c4m/dpop.rs` と `tests/test_c4m/cat.rs` の既存テスト (`key_binding_errors`、`verify_against_token_with_replay_cache`、`builder_setters_are_covered` など) を更新すること。
- `pbt/tests/prop_c4m/cat.rs` の `Confirmation` のモデルが `confirmation_key_3` と解釈に追随し、`cargo test -p pbt` が通ること。

### ドキュメントとビルド

- `docs/c4m.md` と `skills/shiguredo-moqt/SKILL.md` が、SETUP / namespace / track の resource 形式、replay cache の `max_window_seconds`、`cnf` の既定 IANA と C4M draft 互換オプションを説明していること。
- fuzz ターゲットが新しい `verify_resource_consistency(action)` のシグネチャに追随し、`cargo +nightly fuzz build fuzz_verify_c4m_dpop_context` と `cargo +nightly fuzz build fuzz_c4m_authorization_context` が通ること。
- `markdownlint-cli2` が変更した Markdown で通ること。
- 次のコマンドが通ること。
  - `cargo test --workspace`
  - `cargo test -p shiguredo_moqt --features aws-lc-rs`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo clippy -p shiguredo_moqt --all-targets --features aws-lc-rs -- -D warnings`
  - `cargo fmt --all -- --check`
  - `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt --features aws-lc-rs`

## 対象外

- kid ベースの PoP の鍵解決 / 検証。本 issue では `Confirmation::kid` で kid を公開し、サムプリントとの誤比較をなくすところまでを扱う。
- CWT 形式の DPoP proof (`dpop-proof+cwt`)。actx の claim label が TBD のため、従来どおり対象外とする。
- クエリパラメータの重複時の厳密な検証。0356 のモデル独立化で扱う。
- `moqt-reval` の再検証。0345 で扱う。

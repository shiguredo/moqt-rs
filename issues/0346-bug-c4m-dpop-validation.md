# C4M の DPoP / トークン束縛の検証を仕様に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-c4m-dpop-validation
- Polished: {YYYY-MM-DD}

## 目的

C4M-01 の DPoP 検証に 3 つの不備がある。（1）接続確立時の正当な `resource` (`moqt://<relay-endpoint>`) を拒否する、（2）replay cache の保持期間がトークン間で共有されず古い jti を再受理し得る、（3）`cnf` の key 3 を IANA の `kid` ではなく常に jkt として解釈する。認可と replay 防御の正しさに影響する。

## 現状

- `src/c4m/dpop.rs` の `verify_resource_consistency` は `resource` があれば常に `tns` (と `tn`) の一致を要求する。§3.1.3 は接続確立の resource を `moqt://<relay-endpoint>` (クエリ無し) と定義しており、SETUP 用の正当な proof を `ResourceInconsistent` で拒否する。テストも拒否を固定している。
- `src/c4m/dpop.rs` の `DpopReplayCache::check_and_record` は現在リクエストの `window_seconds` で古いエントリを purge する。同一主体で window の異なるトークンを使うと、小さい window で purge した jti を大きい window のトークンで再送して受理され得る。
- `src/c4m/cat.rs` の `Confirmation` は `cnf` の key 3 を常に `c4m_draft_jwk_thumbprint` として扱う。IANA の CWT Confirmation Methods では 3 は `kid`、C4M draft A.4 は 3 を jkt に使うという不整合があり、kid ベースの PoP を区別できない。
- 影響: SETUP の正当な proof を落とす。replay 防御が window の組み合わせで破れる。実装間で cnf の解釈が食い違う。

## 設計方針

- `verify_resource_consistency` に action を渡し、CLIENT_SETUP / SERVER_SETUP ではクエリ無し resource を受理して endpoint の妥当性だけを見る。namespace / track 操作では現行の tns / tn 一致を維持する。
- replay cache の保持期間を最大 window に固定するか、キャッシュをトークン / セッションに紐付ける。
- `cnf` key 3 の解釈を IANA 準拠と C4M draft 互換で切り替えられるようにし、既定と互換オプションを doc に明記する。
- それぞれ positive / negative テストを追加する。

## 完了条件

- SETUP 用 resource が受理されること。
- window の異なるトークン間で jti が再受理されないこと。
- `cnf: {3: kid}` と draft 形式を区別して扱えること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

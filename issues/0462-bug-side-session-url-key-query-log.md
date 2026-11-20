# moq-sub の side session キーが query の認可トークンをログに出す

- Created: 2026-11-20
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-side-session-url-key-query-redaction
- Polished: {YYYY-MM-DD}

## 目的

`crates/moq-sub/src/pipeline.rs` の `side_session_url_key` は、side session の同一性キーとログ表示に使う文字列を `moqt://{authority}{path}#{fragment のハッシュ}` の形で組み立てる。
`tokio_moq::ServerUrl::path` は query を含むため (`crates/tokio-moq/src/lib.rs` の `ServerUrl::path` の doc が「パス部分 (query を含み、fragment を含まない)」と定める)、query に認可トークンを置く運用では、そのまま `tracing::info!` の `url={key}` としてログに残る。

MSF は接続 URI のパラメータとして token を供給してよいと定めており (draft-ietf-moq-msf-02 §11.5.2)、クライアントは `--url` の query を SETUP の PATH option と WebTransport の `:path` にそのまま引き継ぐ。relay が同じ query を含む New Session URI を返す経路は現実的であり、その URL がログに出ると認可トークンが漏れる。

`fragment` は認可トークンを含み得るためハッシュにしているが、同じ理由が query にも当てはまる。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `side_session_url_key` は `format!("moqt://{}{}#{:016x}", url.authority, url.path, hasher.finish())` でキーを作る。`url.path` は query を含むため、`moqt://relay.example/live?token=SECRET` は `moqt://relay.example/live?token=SECRET#...` になる。
- このキーは次のログに実値のまま出る。
  - `Starting a request migration side session: url={key}, request={}`
  - `Sharing the request migration side session: url={key}, state={:?}, request={}`
  - `Replacing a closed request migration side session: url={key}`
  - `Re-issued the request on the request migration side session: url={url}, ...`
  - `The request migration side session ended: url={url}, reason={end_reason}`
- キーは side session の同一性判定にも使う。`tokio_moq::same_session_url` は `authority` / `path` (query を含む) / `fragment` を比較するため、キーから query を落とすと別々の side session にすべき URL が同じキーになる。
- 既存テストの `side_session_url_key_identifies_the_connection_target` は fragment の値がキーに含まれないことだけを固定しており、query は検査していない。
- 影響: query に認可トークンを置く運用で、moq-sub の既定ログフィルタ (info) にトークンが残る。ログは障害調査のために長期保管され得るため、漏えいの影響が広い。

## 設計方針

キーの同一性とログ表示を分離する。

- `side_session_url_key` は、同一性のためのキーとして URL 全体 (authority / path / fragment) を区別できる値を返す。query も含めて区別する。可読な URL 文字列ではなく、`DefaultHasher` による 64 bit のハッシュに統一する案を採る場合は、衝突で別の接続先を同じ side session にしないよう、ハッシュだけでなく元の値でも比較する構造にすること。
- ログに出す値は `?` より前の path と、query のハッシュ、fragment のハッシュに留める。`authority` は userinfo を含み得るため、表示から `@` 以降を除くか、`authority_parts` と同じ検証を通した host / port だけを表示する。
- 変更対象は `crates/moq-sub/src/pipeline.rs`。`crates/moq-pub/src/pipeline.rs` の `side_session_key` は同じ形で `url.path` を連結しているが、現時点でこの値をログに出していない。将来のログ出力で漏れないよう同じ方針に揃えるかは、本 issue の完了条件に含めず、必要なら別 issue にする。

## 完了条件

- `side_session_url_key` が query を含む URL でも認可トークンの実値を含まないこと。
- query だけが異なる URL が別の side session キーになること (side session を共有してはならない)。
- fragment だけが異なる URL が別の side session キーになること (既存の契約を維持する)。
- authority / path の違いが引き続きキーに反映されること。
- `crates/moq-sub/src/pipeline.rs` の単体テストに、`token=SECRETVALUE` のようなダミーの query を付けた URL を追加し、次の両方を固定すること。
  - キーに `SECRETVALUE` が含まれないこと。
  - query が異なる URL のキーが互いに異なること。
- 上記のログ出力箇所がキーを実値のまま出さないこと (コードレビューで確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

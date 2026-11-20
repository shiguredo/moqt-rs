# moq-sub の side session キーが query の認可トークンをログに出す

- Created: 2026-11-20
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-side-session-url-key-query-redaction
- Polished: 2026-11-20

## 目的

`crates/moq-sub/src/pipeline.rs` の `side_session_url_key` は、side session の同一性キーとログ表示に使う文字列を `moqt://{authority}{path}#{fragment のハッシュ}` の形で組み立てる。
`tokio_moq::ServerUrl::path` は query を含むため (`crates/tokio-moq/src/lib.rs` の `ServerUrl::path` の doc が「パス部分 (query を含み、fragment を含まない)」と定める)、query に認可トークンを置く運用では、そのまま `tracing::info!` の `url={key}` としてログに残る。

MSF は接続 URI のパラメータとして token を供給してよいと定めており (draft-ietf-moq-msf-02 §11.5.2)、クライアントは `--url` の query を SETUP の PATH option と WebTransport の `:path` にそのまま引き継ぐ。relay が同じ query を含む New Session URI を返す経路は現実的であり、その URL がログに出ると認可トークンが漏れる。

`fragment` は認可トークンを含み得るためハッシュにしているが、同じ理由が query にも当てはまる。

## 現状

- `crates/moq-sub/src/pipeline.rs` の `side_session_url_key` は `format!("moqt://{}{}#{:016x}", url.authority, url.path, hasher.finish())` でキーを作る。`url.path` は query を含むため、`moqt://relay.example/live?token=SECRET` は `moqt://relay.example/live?token=SECRET#...` になる。
- このキーは side session の `url=` を出すログに実値のまま出る。例えば次のログである。
  - `Starting a request migration side session: url={key}, request={}`
  - `Sharing the request migration side session: url={key}, state={:?}, request={}`
  - `Replacing a closed request migration side session: url={key}`
  - `Re-issued the request on the request migration side session: url={url}, ...`
  - `The request migration side session ended: url={url}, reason={end_reason}`
  - ほかにも media FETCH の再発行、REQUEST_ERROR、GOAWAY、close、`Ignoring a session event`、main ループの終了ログが `url={url}` を出す。
- キーは side session の同一性判定にも使う。`tokio_moq::same_session_url` は `authority` / `path` (query を含む) / `fragment` を比較するため、キーから query を落とすと別々の side session にすべき URL が同じキーになる。
- 既存テストの `side_session_url_key_identifies_the_connection_target` は、秘匿の観点では fragment の値がキーに含まれないことだけを固定しており、query は検査していない。
- 影響: query に認可トークンを置く運用で、moq-sub の既定ログフィルタ (info) にトークンが残る。ログは障害調査のために長期保管され得るため、漏えいの影響が広い。

## 設計方針

キーの同一性とログ表示を分離する。

- `side_session_url_key` は、同一性のためのキーとして URL 全体 (authority / path / fragment) を区別できる可読な文字列を返す。query も含めて区別する。認可トークンの実値をキーに含めないため、query と fragment は実値ではなく `DefaultHasher` によるハッシュを埋め込む。キーは `moqt://{authority}{path}?q={query のハッシュ}#{fragment のハッシュ}` の形にする (path は `?` より前を使い、query や fragment が無いときは対応する部分を省く)。
- ログに出す値は、`?` より前の path、query のハッシュ、fragment のハッシュ、userinfo を除いた authority に留める。`authority` は userinfo (`user:password@host` の `@` より前) を含み得るため、表示では `@` より前を除き、`@` より後ろの host / port だけを表示する。`tokio_moq::authority_parts` は private で userinfo を拒否するため moq-sub からは呼べない。`authority` から `@` より前を落とすだけでよい。
- side session の `url=` を出すすべてのログで、`side_session_url_key` の値ではなく表示用の値を使う。表示用の関数 (`side_session_url_display` など) を追加し、キーは同一性判定とイベントの照合にだけ使う。表示用の関数はキーの文字列から authority の userinfo を除いた文字列を返す。`RequestMigrationEvent` の `url` はキーのままにし、ログだけ表示用の値にする。
- 変更対象は `crates/moq-sub/src/pipeline.rs`。

## 完了条件

- `side_session_url_key` が query を含む URL でも認可トークンの実値を含まないこと。
- query だけが異なる URL が別の side session キーになること (side session を共有してはならない)。
- fragment だけが異なる URL が別の side session キーになること (既存の契約を維持する)。
- authority / path の違いが引き続きキーに反映されること。
- `crates/moq-sub/src/pipeline.rs` の単体テストに、`token=SECRETVALUE` のようなダミーの query を付けた URL を追加し、次の両方を固定すること。
  - キーに `SECRETVALUE` が含まれないこと。
  - query が異なる URL のキーが互いに異なること。
- 表示用の値についても、`SECRETVALUE` と authority の userinfo (`@` より前) が含まれないことを単体テストで固定すること。
- side session の `url=` を出すすべてのログが、キーではなく表示用の値を使うこと (コードレビューで確認する)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

# コメントとドキュメントの言語、表記、issue 参照を規約に適合させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-doc-comment-format-compliance
- Polished: 2026-10-20

## 目的

AGENTS.md は「コメントは全て日本語」「全角と半角の間には半角スペース」を、shiguredo-issues は
「issue 番号・issue への言及をソースコードに持ち込まない」ことを求める。shiguredo-rust は
テストの panic / assert メッセージを日本語、本体コードと `examples/` のログを英語とする。
shiguredo-doc は全角半角の境界、半角括弧、中黒の扱いを定める。現状は英語のみのコメントと
テストメッセージ、全角半角間のスペース欠落、draft の changelog PR 番号の混入がある。
これらを規約に適合させ、コメントから仕様の根拠や設計意図を辿れる状態にする。

## 対象

- `src/`, `tests/`, `pbt/`, `examples/` の `//` / `///` / `//!` コメント
- `tests/`, `pbt/`, `src/` の `#[cfg(test)]` モジュールの panic / assert メッセージ
  (`unreachable!` / `expect` / `assert!` など)
- `docs/`, `skills/` の日本語文の全角半角境界、全角中黒、全角括弧
- 対象外: 仕様の英語引用、`# Errors` などの rustdoc 見出し、コード例・表・コード断片、
  本体コードと `examples/` のログ / `println!` / `eprintln!` メッセージ、
  `docs/` の issue 参照 (0323 で扱う)

## 現状

- 英語のみのコメントが `tests/`, `pbt/`, `examples/` に多数ある。代表例:
  `tests/test_track_properties.rs` の「delta=1 (odd type), length=10, but only 3 bytes of data」、
  `tests/test_message_parameter.rs` の「delta = 2 (type=0x02 - prev=0)」、
  `examples/tokio-moq/src/webtransport_h2.rs` の「1. TCP + TLS」。
  仕様の英語引用 (`draft-ietf-...` / `§...`) は対象外とする。
- `tests/`, `pbt/`, `src/` の `#[cfg(test)]` モジュールの panic / assert メッセージにも
  英語のみのものがある。代表例: `pbt/tests/prop_varint.rs` の
  `unreachable!("vi64 encoded length is always 1..=9")`、`src/session/types.rs` の
  `terminated_subscription_with_publish_done` テストの
  `expect("terminated_subscription_with_publish_done must set publish_done")`。
- 全角半角間のスペース欠落が `src/`, `tests/`, `pbt/`, `examples/`, `docs/`, `skills/` にある。
  `・` と ASCII の間が 83 箇所、全角括弧と ASCII の間が 32 箇所
  (2026-10-20 時点、`rg` で確認)。全角括弧は `src/` と `tests/` のコメントに 116 箇所あり、
  半角 `()` に統一する。`src/message_parameter.rs` の `（EXPIRES=0 のとき）`、
  `src/session/types.rs` の `§9.20.16（将来の draft で変更される可能性がある）` など。
- draft の changelog PR 番号 (`#1595`, `#1583`, `#1540`, `#1790`, `#1872`, `#1623`) が
  `src/` と `tests/`, `pbt/` の 18 ファイルにある。`src/varint.rs`, `src/message.rs`,
  `src/message_parameter.rs`, `src/subgroup_tracker.rs` はその一部。
  `rg -n '#[0-9]{3,5}' src tests pbt` で全数を確認できる。
- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` に
  「本 issue の対象外」というコメントがある。
- 影響: 規約違反であり、draft の changelog PR 番号は draft の版が変わると辿れなくなる。

## 設計方針

- 英語のみのコメントを日本語にする (手順説明、テストデータの説明、コードの意図を含む)。
  仕様の英語引用は `"..."` で囲み、直前または直後に `draft-ietf-moq-transport-22 §...` の
  形で引用元を付けて残す。
- テストの panic / assert メッセージは日本語にする。本体コードと `examples/` のログ /
  `println!` / `eprintln!` メッセージは英語のままとする (`shiguredo-rust`)。
- 全角と半角の境界に半角スペースを入れる。全角括弧 `（）` は半角 `()` に統一し、前後に
  半角スペースを入れる (短い補足注記では閉じ括弧の後ろにスペースを入れない)。
  全角中黒 `・` と ASCII が隣接する箇所は、並列列挙なら 2 項は
  半角スラッシュ `/`、3 項以上は読点 `、` に置き換える。固有名詞内部の中黒は残し、ASCII との
  境界に半角スペースを入れる。`docs/` と `skills/` では、ASCII が隣接しない並列列挙の `・`
  も `/` または `、` に置き換える (`shiguredo-doc`)。
- source コメントからこのリポジトリの issue 番号・issue パス・「本 issue」への言及を削除し、
  理由そのもの (仕様の節番号・再現条件・設計意図) に置き換える。draft の changelog PR 番号は
  draft の版が変わると辿れなくなるため削除し、
  `draft-ietf-moq-transport-22 Appendix A.x (Since draft-ietf-moq-transport-y)` の形に置き換える。
- `# Errors` などの rustdoc 見出しは現行のプロジェクト慣例を維持し、本 issue では変更しない。

## 完了条件

- 対象のコメントとテストメッセージが日本語であること。仕様の英語引用は引用と分かる形
  (`"..."` と `draft-ietf-moq-transport-22 §...`) で残っていること。`# Errors` などの
  rustdoc 見出しは現行のままであること。
- 対象のコメントに全角括弧・全角中黒・全角半角間のスペース欠落が無いこと。
  `rg -n '//.*[（）]' src tests pbt examples`、
  `rg -n '//.*[A-Za-z0-9]・|//.*・[A-Za-z0-9]' src tests pbt examples`、
  `rg -n '[A-Za-z0-9]・|・[A-Za-z0-9]' docs skills`、
  `rg -n '[（）]' docs skills` が 0 件であること。
- docs/skills の並列列挙に `・` が無いこと (固有名詞内部を除く)。
- ソースコメント・テストコメントにこのリポジトリの issue 番号 / issue パス /
  「本 issue」への言及が無いこと。draft の changelog PR 番号も無いこと。
  `rg -n '//.*#[0-9]{3,5}|//.*本 issue|//.*issues/' src tests pbt examples` が 0 件であること。
- 本体コードと `examples/` のログ / `println!` / `eprintln!` メッセージが英語のままであること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

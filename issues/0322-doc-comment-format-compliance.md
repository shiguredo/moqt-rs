# コメントの言語・表記・issue 参照を規約に適合させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-comment-format-compliance
- Polished: {YYYY-MM-DD}

## 目的

AGENTS.md は「コメントは全て日本語」「全角と半角の間には半角スペース」を、shiguredo-issues は「issue 番号・issue への言及をソースコードに持ち込まない」ことを求める。現状は英語のみのコメント、全角半角間のスペース欠落、draft の changelog PR 番号の混入がある。

## 現状

- 英語のみのコメントが tests 約 170 行、pbt 約 19 行、examples 約 59 行ある。代表例: `tests/test_track_properties.rs` の「delta=1 (odd type)...」、`tests/test_message_parameter.rs` の「delta = 2 (type=0x02 - prev=0)」、`pbt/tests/prop_varint.rs` の
  `unreachable!("vi64 encoded length is always 1..=9")`、`examples/tokio-moq/src/webtransport_h2.rs` の「1. TCP + TLS」。
- 全角半角間のスペース欠落が `src/message_parameter.rs` と `src/session/types.rs` のコメント、`skills` / `examples` / `docs` の全角中黒と ASCII の間などに約 40 箇所ある。
- `src/varint.rs`, `src/message.rs`, `src/message_parameter.rs`, `src/subgroup_tracker.rs` のコメントに draft の changelog PR 番号 (`#1595` など) が入っている。`examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` には「本 issue の対象外」というコメントがある。
- 影響: 規約違反であり、issue 番号は将来辿れなくなる。

## 設計方針

- 手順説明の英語コメントを日本語にする。仕様の英語引用は引用と分かる形で残す。
- 全角半角の境界に半角スペースを入れる。全角括弧・全角中黒の扱いを統一する。
- source コメントから issue 番号と issue 参照を削除し、理由そのもの (仕様の節番号・再現条件・設計意図) に置き換える。

## 完了条件

- 対象ファイルのコメントが日本語・全角半角スペース規約に適合していること。
- ソースコメント・テストコメントに issue 番号 / issue 参照が無いこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

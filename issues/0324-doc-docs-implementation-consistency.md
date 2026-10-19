# docs / README / SKILL.md を実装に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/doc-docs-implementation-consistency
- Polished: {YYYY-MM-DD}

## 目的

docs と SKILL.md / README の記述が実装と食い違い、利用者やエージェントに誤った API を案内している。実装を正としてドキュメントを修正する。

## 現状

- docs/moqt.md は「音声の時間圧縮・伸長」を機能として書くが、`src/playout/stretch.rs` は時間伸長を持たないと明記し、公開 API は `compress` / `conceal` だけである。skills/shiguredo-moqt/SKILL.md も「時間伸縮」と書く一方で「時間伸長は持たない」と自己矛盾している。
- SKILL.md の playout 説明は `PlayoutBuffer::select` の選択規則、時間圧縮の相関窓、A/V 同期条件、モジュール表、timeline の API 一覧が実装と一致しない。
- examples/moq-sub/README.md、examples/moq-pub/src/error.rs、examples/moq-sub/src/pipeline.rs が存在しない `--audio-datagram` を案内している (実在するのは `--audio-subgroup`)。
- README.md の C4M トークンを載せる制御メッセージの列挙に FETCH が無い。
- examples/moq-pub/README.md の `--audio-subgroup` の説明に `--datagram-max-size` の上限説明が混ざっている。
- examples/moq-sub の `run` の doc コメントが `video_subscribe_parameters` の直前に取り残されている。
- `--namespace` は省略可能なのに help の Usage が必須として表示する。moq-pub は `Cargo.toml` に description が無く、`CARGO_PKG_DESCRIPTION` 由来の説明行が出ない。
- Cargo.toml の `include` が docs / examples を含まないため、README の相対リンクが crates.io / docs.rs で壊れる。

## 設計方針

- 実装の API と挙動から記述を再生成する。
- 存在しないフラグ・古い API 名を削除する。
- README のリンクを GitHub の絶対 URL にするか、package に docs を含める。

## 完了条件

- 上記の記述が実装と一致していること。
- `--audio-datagram` などの存在しないフラグの案内が無いこと。
- README のリンクが crates.io / docs.rs でも解決すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

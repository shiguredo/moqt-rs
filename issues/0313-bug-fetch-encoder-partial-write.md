# FetchStreamEncoder::encode_object がエラー時に出力バッファへ部分書き込みしないようにする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-encoder-partial-write
- Polished: {YYYY-MM-DD}

## 目的

公開 API の encode 系は「検証に失敗した場合、呼び出し側の出力バッファに部分バイトを残さない」契約で揃っている。`FetchStreamEncoder::encode_object` だけが検証前にエントリ全体を書き込むため、エラー時に不正なバイト列がバッファに残り、呼び出し側がバッファを再利用すると壊れた wire を作る。

## 現状

- `src/stream/encoder.rs` の `encode_object` は `FetchStreamEntry::encode` で `buf` に書き込んだ後、同一 Subgroup 内の Publisher Priority 一貫性 (§12.1 条件 1) を検証して `Err` を返す。
- 失敗時は `prior_state` / `prior_context` を更新しないが、`buf` には書き込んだバイト列が残る。
- `SubgroupObject::encode` / `FetchStreamObject::encode` は書き込み前に全検証を行い、失敗時に `buf` を変化させないことをテストで固定している。`FetchStreamEncoder` だけが規約から外れている。
- 影響: `buf` を再利用する利用者で、エラー後に壊れた Object が送信され得る。

## 設計方針

- Priority 一貫性の検証を `entry.encode` より前に移動する。難しければローカルの一時バッファへ encode し、成功時だけ `buf` へ追記する。
- 他のエラー経路でも `subgroup_priorities` を先に更新しないことを確認する。
- 失敗時に `buf` が不変であることをテストで固定する。

## 完了条件

- `encode_object` のすべてのエラー経路で `buf` が変化しないこと。
- 上記を `tests/test_stream/encoder.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

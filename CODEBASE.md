# CODEBASE

- **クライアントとサーバーのみでリレーには対応しないこと**

- 良い設計は良いコードを生む
- より良い設計のためには破壊的変更を恐れないこと
- 徹底的に評価をしていく
- 徹底的に時間をかけて継続的に改善をしていく
- 破壊的変更は積極的に行う
- 設計変更も積極的に行う
- 調査メモなどは docs/ 以下に保存していく
- 間違いを認めよりよい修正をする

## ルール

- この指示がなくなるまでは GitHub Actions 対応はしないこと
- この指示がなくなるまでは WebAssembly 対応をしないこと
- この指示がなくなるまでは変更履歴を `CHANGES.md` に残さないこと
- この指示がなくなるまでは Pull-Request やブランチを作らず develop にコミットしていくこと
  - 1 issue 1 commit 1 push

## Rust

- Sans I/O を徹底すること
- no_std で実装すること

## MOQT

- 最新ドラフトに準拠すること
- 実 relay が必要な E2E テストは `#[ignore]` を付け、CI が `--ignored` を付けて必ず実行すること (`e2e-tests/`)。通常の `cargo test` では実行しない
- `Session` (State Machine) は 1 本の `MOQT Transport Session` に閉じた protocol state machine として設計すること
- `Session` は 1 peer 間の endpoint-local な state machine として扱うこと
- MOQT draft の issue は draft 番号を含めて {SEQUENCE}-draft-{NUM}-{short-description}.md という命名規則で作成すること
  - 例: `0001-draft-18-support-for-joins.md`
- **リレーの実装について書いてよいのは「Sora の Media over QUIC 実装であり、リレー機能を提供する」ということだけである**

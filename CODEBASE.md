# CODEBASE

- **クライアントとサーバーのみでリレーには対応しないこと**
- より良い設計のためには破壊的変更を恐れないこと
- Sans I/O を徹底すること
- no_std で実装すること
- 最新ドラフトに準拠すること
- 実 relay が必要な E2E テストは `#[ignore]` を付け、CI が `--ignored` を付けて必ず実行すること (`e2e-tests/`)。通常の `cargo test` では実行しない
- 最小 Rust バージョン (MSRV) はクレートごとに実際の依存要求で宣言すること
  - 既定は `shiguredo-rust` 規約どおり 1.93 とする (`shiguredo_moqt` / `tokio-moq` / `moq-sub`)
  - `moq-pub` と、それに依存する `e2e-tests` は 1.94 に引き上げている (依存する `raden` と `cranelift-codegen` が 1.94 を要求するため)
  - 引き上げ要因のないクレートまで値を揃えないこと。ライブラリ本体の受け皿を狭めるため
  - `Cargo.toml` の `rust-version` を変えたらルート `README.md` と `examples/README.md` の前提条件が追従しているか確認すること
- `Session` (State Machine) は 1 本の `MOQT Transport Session` に閉じた protocol state machine として設計すること
- `Session` は 1 peer 間の endpoint-local な state machine として扱うこと
- MOQT draft の issue は draft 番号を含めて {SEQUENCE}-draft-{NUM}-{short-description}.md という命名規則で作成すること
  - 例: `0001-draft-18-support-for-joins.md`
- **リレーの実装について書いてよいのは「Sora の Media over QUIC 実装であり、リレー機能を提供する」ということだけである**

## ルール

- この指示がなくなるまでは Pull-Request を作らずブランチ作成して対応して、CI 通ったら develop にスカッシュマージしていくこと

# shiguredo_moqt

- より良い設計のためには破壊的変更を恐れないこと
- Sans I/O を徹底すること
- no_std で実装すること
- 最新ドラフトに準拠すること
- 最小 Rust バージョン (MSRV) はクレートごとに実際の依存要求で宣言すること
  - 既定は `shiguredo-rust` 規約どおり 1.93 とする (`shiguredo_moqt` / `moqt-example-transport` / `moqt-subscriber`)
  - `moqt-publisher` だけ 1.94 に引き上げている (依存する `raden` と `cranelift-codegen` が 1.94 を要求するため)
  - 引き上げ要因のないクレートまで値を揃えないこと。ライブラリ本体の受け皿を狭めるため
  - `Cargo.toml` の `rust-version` を変えたらルート `README.md` と `examples/README.md` の前提条件が追従しているか確認すること
- `Session` (State Machine) は 1 本の `MOQT Transport Session` に閉じた protocol state machine として設計すること
- `Session` は 1 peer 間の endpoint-local な state machine として扱うこと
- MOQT draft の issue は draft 番号を含めて {SEQUENCE}-draft-{NUM}-{short-description}.md という命名規則で作成すること
  - 例: `0001-draft-18-support-for-joins.md`

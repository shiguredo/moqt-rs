# no_std ビルドと rustdoc 検査を CI に追加する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/test-add-no-std-and-rustdoc-ci

## 目的

`CODEBASE.md`「no_std で実装すること」と doc の健全性を CI で継続的に検証する。

## 現状

`.github/workflows/ci.yml` は prek (fmt / clippy 等) / `cargo test --workspace` / fuzz build のみを実行し、次の検証が無い。

- no_std ビルド: `cargo build --target thumbv7em-none-eabihf -p shiguredo_moqt` は成功するが CI では未検証。`nojson` の default feature 無効化 (`Cargo.toml`) 等が崩れても検出できない。
- rustdoc: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が未解決 intra-doc link 21 件で失敗するが、CI では検出されない。

## 設計方針

CI に no_std ビルドと rustdoc 検査のステップ (またはジョブ) を追加する。runner は既存の `macos-15` に合わせる。rustdoc の失敗は別 issue で修正する。

## 完了条件

- CI で no_std ビルドが検証されること
- CI で rustdoc 検査が実行されること
- 既存ジョブの実行時間が過度に増えないこと

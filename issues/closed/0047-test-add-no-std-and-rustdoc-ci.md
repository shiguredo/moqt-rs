# no_std ビルドと rustdoc 検査を CI に追加する

- Created: 2026-09-10
- Completed: 2026-09-10
- Branch: feature/test-add-no-std-and-rustdoc-ci
- Polished: 2026-09-10

## 目的

`CODEBASE.md`「no_std で実装すること」と doc の健全性を CI で継続的に検証する。

## 現状

`.github/workflows/ci.yml` のジョブは prek（fmt / clippy 等）/ `cargo test --workspace` / fuzz build の 3 つで、次の検証が無い。

- no_std ビルド: `cargo build --target thumbv7em-none-eabihf -p shiguredo_moqt` は成功するが CI では未検証。`nojson` の default feature 無効化（`Cargo.toml` の `nojson = { version = "0.3", default-features = false }`）が崩れても検出できない。
- rustdoc: `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` が未解決 intra-doc link 21 件で失敗するが、CI では検出されない。

## 設計方針

- no_std ビルドと rustdoc 検査を行うジョブを 1 つ追加する。対象は `-p shiguredo_moqt` に限定するため examples の macOS 専用依存をビルドせず、runner は `macos-15` を必須としない。
- runner は `shiguredo-github-actions` 規約に従い `ubuntu-slim` を第一候補とする。`ubuntu-slim` に Rust が無い場合は `ubuntu-latest` を使い、その理由をワークフローのコメントに書く。
- no_std ビルドの前に `rustup target add thumbv7em-none-eabihf` を実行する（GitHub-hosted runner には未導入）。
- rustdoc 検査は `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` で行う。
- 新規ジョブを追加し、既存ジョブの構成は変えない。
- `.github/workflows/ci.yml` 冒頭の「runner は macos-15 を使う」コメントは全ジョブを対象にした記述のため、既存 3 ジョブは workspace 全体を対象に macos-15、新規ジョブは `-p shiguredo_moqt` 限定で Linux 系 runner、とスコープを書き分けて更新する。

## 依存

- 0046 の修正を先に取り込む。`-D warnings` 付き rustdoc 検査は、0046 が未完了だと未解決 link 21 件で失敗し CI が赤くなる。0046 が develop に入った後に本 issue を実装する。

## 完了条件

- CI に no_std ビルド（`-p shiguredo_moqt --target thumbv7em-none-eabihf`）が追加され、成功すること
- CI に rustdoc 検査（`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt`）が追加され、成功すること
- 追加したジョブが既存ジョブと同じく PR で実行されること
- 既存ジョブの構成を変えず、ジョブ数を 1 つだけ増やすこと

## 解決方法

no_std ビルドと rustdoc 検査を CI に追加した。

- `no-std-doc` ジョブを `ubuntu-latest` で追加した。`rustup target add thumbv7em-none-eabihf` → `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` → `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` を実行する。
- runner は `shiguredo-github-actions` 規約に従い `ubuntu-slim` を第一候補としたが、Rust (rustup) が無いため `ubuntu-latest` を使う。理由を ci.yml 冒頭コメントに書いた。
- 既存 3 ジョブは変更せず、ジョブ数を 1 つ追加した。ci.yml 冒頭コメントをジョブごとの目的・runner スコープに整理した。
- `CHANGES.md` の `### misc` に `[UPDATE]` エントリを追加した。

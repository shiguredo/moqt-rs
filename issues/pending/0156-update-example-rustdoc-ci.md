# example クレートの rustdoc を CI で検査する

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/update-example-rustdoc-ci
- Polished: 2026-09-28
- Updated: 2026-11-14

## pending にした理由

`CODEBASE.md` のルール「この指示がなくなるまでは GitHub Actions 対応はしないこと」が
2026-11-14 時点でも残っており、CI ジョブ (または既存ジョブへのステップ) の追加が
この方針に反すると解釈され得る。この行を削除する
`issues/0325-doc-codebase-github-actions-consistency.md` が未完了のため、本 issue は
pending にする。0325 の完了後に `issues/` へ戻し、
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` を macos-15 のジョブに追加する。

## 目的

example クレートの rustdoc の警告を CI で検出する。`no-std-doc` ジョブは `shiguredo_moqt` だけを対象にしているため、example 側の private item への intra-doc link の警告が CI も prek も通ってしまう。

## 現状

`.github/workflows/ci.yml` の `no-std-doc` ジョブは ubuntu-latest で
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` を実行し、併せて
`cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf` で no_std ビルドを確認する。
同じ `ci.yml` の `test` ジョブ (macos-15) には `aws-lc-rs` feature 版の rustdoc 検査
(`cargo doc --no-deps -p shiguredo_moqt --features aws-lc-rs`) があるが、feature 依存の検査のため
workspace 全体の検査を代替しない。
`crates/` の 3 クレート (`tokio-moq` / `moq-pub` / `moq-sub`) のうち `moq-pub` は macOS 専用依存 (`shiguredo_audio_device` / `shiguredo_video_device`) を無条件の依存に持つため ubuntu ではビルドできず、`cargo doc --no-deps --workspace` は macos-15 でしか実行できない
(`tokio-moq` 単体は Linux でもビルド可能だが、workspace 全体では `moq-pub` がビルドできないため実行できない。`moq-sub` の macOS 専用依存は `[target.'cfg(target_os = "macos")']` でゲートされている)。`prek.toml` にも
`cargo doc` のフックは無い。

このため、0132 (`issues/closed/0132-bug-url-default-port.md`) の実装中に公開 doc から private の `DEFAULT_MOQT_PORT` (`crates/tokio-moq/src/lib.rs` の private `const`) への intra-doc link が混入し、同 issue のレビューで発見されたが、既存の CI ジョブでも
`prek` でも検出できなかった (発見は `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` を手で実行したとき)。現行 develop では公開 doc からの当該リンクは解消されているが
(private item の doc コメント内のリンクのみ残っている)、同種の問題が再混入しても CI も prek も検出しない。

また現行 develop には、`tokio-moq` の rustdoc 警告 10 件
(private item へのリンク、未解決リンク、冗長なリンク。2026-11-14 時点の Linux での実測) と、
`shiguredo_moqt` の `src/session/fetch.rs` の冗長なリンク警告 1 件が残っている。
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p tokio-moq` と
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` は現状で失敗し、
`cargo doc --no-deps --workspace` を `-D warnings` で実行しても失敗する。

なお `CODEBASE.md` は「この指示がなくなるまでは GitHub Actions 対応はしないこと」を定めており (2026-10-10 の整備で追記)、CI ジョブを追加する本 issue は現時点では着手できない。

## 設計方針

- macos-15 で動くジョブ (既存の `test` ジョブ、または新しい example-doc ジョブ) に `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` を追加する
- 現行 develop の rustdoc 警告 (`tokio-moq` 10 件、`shiguredo_moqt` 1 件) を 0 件にしてから CI を追加する。警告は `#[allow]` で抑制せず、リンクの修正で解消する
- `no-std-doc` ジョブは no_std ビルドの確認を兼ねるため残す。`shiguredo_moqt` の doc 検査が ubuntu 側と重複するが、ジョブの目的が異なる
- `fuzz` は独立 workspace (`Cargo.toml` の `exclude = ["fuzz"]`) のため対象外にする
- ジョブを追加する場合、または既存ジョブにステップを追加する場合は、`prek` / `test` / `no-std-doc` との実行時間の関係をコメントに残す

## 完了条件

- macos-15 のジョブで、crates 配下のクレートを含む workspace 全体の rustdoc 警告があるとジョブが失敗すること
- 公開 doc から private item への intra-doc link (例: `DEFAULT_MOQT_PORT`) を一時的に追加すると CI が失敗し、取り除くと成功することを確認すること
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` が警告 0 件で成功すること (既存の `shiguredo_moqt` / `tokio-moq` の警告を解消した後)
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` と `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt --features aws-lc-rs` が成功すること (既存の `no-std-doc` / `test` ジョブの rustdoc 検査)
- CI のジョブ構成を変えた場合は `.github/workflows/ci.yml` の冒頭コメントを更新すること

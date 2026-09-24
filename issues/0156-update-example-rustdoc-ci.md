# example クレートの rustdoc を CI で検査する

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/update-example-rustdoc-ci
- Polished: {YYYY-MM-DD}

## 目的

example クレートの rustdoc の警告を CI で検出する。`no-std-doc` ジョブは `shiguredo_moqt` だけを対象にしているため、example 側の private intra-doc link の警告が CI も prek も通ってしまう。

## 現状

`.github/workflows/ci.yml` の `no-std-doc` ジョブは ubuntu-latest で `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` を実行する。`examples/` の 3 クレート (`moqt-example-transport` / `moqt-publisher` / `moqt-subscriber`) は macOS 専用依存を持つため ubuntu ではビルドできず、対象外になっている。`prek.toml` にも `cargo doc` のフックは無い。

このため公開 doc が private の `DEFAULT_MOQT_PORT` にリンクしていた問題 (0132 の実装中に混入し、同 issue のレビューで発見) は、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p moqt-example-transport` を手で実行するまで検出できなかった。

## 設計方針

- macos-15 で動くジョブ (既存の `test` ジョブ、または新しい example-doc ジョブ) に `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` を追加する
- `no-std-doc` ジョブは no_std ビルドの確認を兼ねるため残す。`shiguredo_moqt` の doc 検査が ubuntu 側と重複するが、ジョブの目的が異なる
- `fuzz` は独立 workspace のため対象外にする (`fuzz-build` と同じ扱い)
- ジョブを追加する場合は、`prek` / `test` / `no-std-doc` との実行時間の関係をコメントに残す

## 完了条件

- macos-15 のジョブで example を含む workspace 全体の rustdoc 警告が失敗になること
- private item への intra-doc link を戻すと CI が失敗することを確認すること
- `cargo doc --no-deps --workspace` が既存の 4 ジョブと同じく成功すること
- CI のジョブ構成を変えた場合は `.github/workflows/ci.yml` の冒頭コメントを更新すること

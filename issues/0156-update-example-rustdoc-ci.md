# example クレートの rustdoc を CI で検査する

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/update-example-rustdoc-ci
- Polished: 2026-09-28

## 目的

example クレートの rustdoc の警告を CI で検出する。`no-std-doc` ジョブは `shiguredo_moqt` だけを対象にしているため、example 側の private item への intra-doc link の警告が CI も prek も通ってしまう。

## 現状

`.github/workflows/ci.yml` の `no-std-doc` ジョブは ubuntu-latest で `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` を実行する。
`examples/` の 3 クレート (`tokio-moq` / `moq-pub` / `moq-sub`) のうち `moq-pub` と `moq-sub` は macOS 専用依存 (Video Toolbox / CoreAudio 系の
`shiguredo_*_device` 等) を持つため ubuntu ではビルドできず、`cargo doc --no-deps --workspace` は macos-15 でしか実行できない
(`tokio-moq` 単体は Linux でもビルド可能だが、workspace 全体では `moq-pub` / `moq-sub` がビルドできないため実行できない)。`prek.toml` にも
`cargo doc` のフックは無い。

このため、0132 の実装中に公開 doc から private の `DEFAULT_MOQT_PORT` への intra-doc link が混入し、同 issue のレビューで発見されたが、既存の CI ジョブでも
`prek` でも検出できなかった (発見は `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` を手で実行したとき)。現行 develop では公開 doc からの当該リンクは解消されているが
(private item の doc コメント内のリンクのみ残っている)、同種の問題が再混入しても CI も prek も検出しない。

## 設計方針

- macos-15 で動くジョブ (既存の `test` ジョブ、または新しい example-doc ジョブ) に `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --workspace` を追加する
- `no-std-doc` ジョブは no_std ビルドの確認を兼ねるため残す。`shiguredo_moqt` の doc 検査が ubuntu 側と重複するが、ジョブの目的が異なる
- `fuzz` は独立 workspace (`Cargo.toml` の `exclude = ["fuzz"]`) のため対象外にする
- ジョブを追加する場合は、`prek` / `test` / `no-std-doc` との実行時間の関係をコメントに残す

## 完了条件

- macos-15 のジョブで example を含む workspace 全体の rustdoc 警告が失敗になること
- 公開 doc から private item への intra-doc link (例: `DEFAULT_MOQT_PORT`) を一時的に追加すると CI が失敗し、取り除くと成功することを確認すること
- `cargo doc --no-deps --workspace` が既存の 3 ジョブ (prek / test / no-std-doc) と同じく成功すること
- CI のジョブ構成を変えた場合は `.github/workflows/ci.yml` の冒頭コメントを更新すること

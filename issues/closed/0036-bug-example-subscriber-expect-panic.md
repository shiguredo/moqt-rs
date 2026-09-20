# moqt-subscriber の raw_player 初期化を Result 化する

- Created: 2026-09-10
- Completed: 2026-09-11
- Branch: feature/fix-example-subscriber-expect-panic
- Polished: 2026-09-11

## 目的

映像デバイスや SDL が無い環境で moqt-subscriber が panic せず、エラーメッセージと終了コードで失敗するようにする。

## 現状

`examples/moqt-subscriber/src/main.rs` は `raw_player::init()` / `VideoPlayer::new()` / `play()` を `expect` で処理している。SDL や映像デバイスが無い環境で panic する。他の example はエラーを `Result` で返しており、非対称。

## 設計方針

`Result` 化して `main` でログ出力し、終了コードを返す。既存のエラー型 (`examples/moqt-subscriber/src/error.rs`) に初期化エラーを追加する。

## 完了条件

- 映像デバイス初期化失敗時に panic しないこと
- 失敗理由がログに英語で出て、非ゼロ終了コードで終わること
- 正常時の動作が維持されること

## 解決方法

`run_raw_player` を `Result` 化し、raw_player の失敗を panic ではなくエラーとして扱うようにした。

- `raw_player::init()` / `VideoPlayer::new()` / `play()` の `expect` を `?` に置き換えた。
- `examples/moqt-subscriber/src/error.rs` に `Error::Player(raw_player::Error)` と `From<raw_player::Error>` を追加した。
- `main` は `run_raw_player` のエラーを `tracing::error!("Fatal: {e}")` で出力し、`std::process::exit(1)` で終了する。
- 失敗時は `raw_player::quit()` を呼ばない (SDL リソース drop 前に呼ばない契約を維持)。
- 正常系は「play 成功後に `video_player` へ保存する」形に整理した。
- `CHANGES.md` の `[FIX]` にエントリを追加した。
- `SDL_VIDEO_DRIVER=invalid` で `Fatal: player: SDL error: invalid not available` と終了コード 1 を実測し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。

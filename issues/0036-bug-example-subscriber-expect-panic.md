# moqt-subscriber の raw_player 初期化を Result 化する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-subscriber-expect-panic

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

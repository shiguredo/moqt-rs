# moqt-subscriber の tokio runtime 構築失敗で終了コード 0 にならないようにする

- Created: 2026-09-12
- Completed: 2026-09-17
- Branch: feature/fix-subscriber-runtime-expect
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-subscriber/src/main.rs` の tokio runtime 構築失敗時に、panic したスレッドだけが落ちて main が終了コード 0 で終わる穴をなくし、起動失敗が非ゼロ終了コードで伝わるようにする。

## 現状

`examples/moqt-subscriber/src/main.rs` は別スレッド内で `tokio::runtime::Runtime::new().expect("failed to create tokio runtime")` を呼んでいる。構築に失敗するとそのスレッドだけが panic し、main スレッドはメディアチャネルの切断検出でループを抜けて `Ok(())` を返すため、終了コード 0 で終わる。0036 で raw_player の初期化失敗は非ゼロ終了になったが、tokio runtime 構築失敗は同種の穴として残っている。

## 設計方針

- runtime 構築を main スレッド側で行うか、構築失敗をチャネルで main に伝えて、失敗時に英語ログと `std::process::exit(1)` で終了する。
- 正常時の動作 (macOS のメインスレッド制約で SDL を main、MoQT 処理を別スレッドで実行) は維持する。

## 完了条件

- runtime 構築失敗時に panic ではなく非ゼロ終了コードで終わること
- 正常時の動作が維持されること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

`examples/moqt-subscriber/src/main.rs` で tokio runtime の構築をメインスレッドへ移した。

- `tokio::runtime::Runtime::new()` を `std::thread::spawn` の外で実行し、失敗したら
  `tracing::error!("Failed to create tokio runtime: {e}")` を出して
  `std::process::exit(1)` する
- 構築に成功した runtime をスレッドへ move し、`rt.block_on(...)` で MoQT 処理を動かす
  (macOS のメインスレッド制約で SDL を main、MoQT 処理を別スレッドに置く構成は維持)

これにより、runtime 構築失敗時に panic したスレッドだけが落ちて main が終了コード 0 で
終わる穴が塞がった。

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` の通過と、relay を経由した end-to-end の
publisher / subscriber 動作 (video / audio の受信、PUBLISH_DONE の受信、正常終了) で
確認した。

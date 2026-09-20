# moq-pub / moq-sub を lib としても使えるようにする

- Created: 2026-10-04
- Completed: 2026-10-04
- Branch: feature/refactor-example-libs
- Polished: {YYYY-MM-DD}

## 目的

`examples/moq-pub` と `examples/moq-sub` はバイナリ専用のクレートで、`src/main.rs` がモジュールを宣言しているため外部のクレートからパイプラインを起動できない。実 relay に対する publish / subscribe の往復を検証する E2E テストは、現状バイナリを起動してログを文字列照合する方法しか取れず、判定がログの文面に依存してしまう。lib ターゲットを公開してパイプラインをプログラムから起動できるようにし、型に基づく検証を可能にする。

## 現状

- `examples/moq-pub/src/main.rs` は `mod pipeline;` のようにモジュールを宣言し、`main()` から `pipeline::run(config, task_monitor, shutdown_monitor)` を呼ぶ。lib ターゲットが無いため、`pipeline` や `cli` はクレート外から使えない
- `examples/moq-sub/src/main.rs` も同様に、`pipeline::run(config, frame_tx, audio_tx, task_monitor, shutdown_monitor, display_backlog, player_stop)` を呼ぶ。デコード済みフレームは `std::sync::mpsc` のチャネルで受け取る
- `cli::Config` は公開フィールドを持つ構造体だが、`cli` モジュールが非公開のためクレート外からは構築できない
- バイナリ固有の処理 (tracing の初期化、Ctrl+C の待ち受け、tokio ランタイムの構築、macOS の SDL プレイヤー) は `main.rs` に閉じている
- どちらのクレートも `publish = false` で、workspace のメンバーである

## 設計方針

- 両クレートに `src/lib.rs` を追加し、外部から使うモジュールを公開する。`main.rs` は lib を利用する形に変更する
- lib には「呼び出し側が用意した shutdown とチャネルでパイプラインを動かす」入口を用意する。tracing の初期化、Ctrl+C、SDL プレイヤーといったバイナリ固有の都合は lib に持ち込まない
- 公開する範囲は E2E テストが必要とするものに限る。内部実装のモジュールは非公開のままにする
- 既存の CLI 引数、ログ出力、終了コード、graceful shutdown の挙動は変えない
- 依存クレートは追加しない

## 完了条件

- `cargo build -p moq-pub -p moq-sub`、`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ること
- 外部クレートから `moq_pub::cli::Config` と `moq_pub::pipeline::run`、`moq_sub::cli::Config` と `moq_sub::pipeline::run` を利用できること
- バイナリの挙動 (CLI 引数、ログ、終了コード) が変わらないこと
- 実 relay に対する publisher / subscriber の起動が lib 経由でも成立すること

## 解決方法

- `examples/moq-pub` と `examples/moq-sub` に `src/lib.rs` を追加し、呼び出し側が必要とするモジュールだけを公開した。
  - `moq-pub` は `cli` / `error` / `pipeline` を公開し、`decoder` などパイプラインの内部実装は非公開のままにした
  - `moq-sub` は `cli` / `error` / `pipeline` を公開し、`decoder` は非公開にして `DecodedVideoFrame` / `DecodedAudioFrame` をクレート直下へ再公開した
- `main.rs` はモジュールを宣言せず lib を利用する形にし、tracing の初期化、Ctrl+C の待ち受け、タスクメトリクスのログ出力、tokio ランタイムの構築、フレームチャネルの準備、SDL プレイヤーの実行、終了コードの決定だけを持つようにした。CLI 引数、ログ出力、終了コード、graceful shutdown の挙動は変えていない。
- `cli::parse_args` を追加し、プログラム名を含めないオプションの配列から `Config` を組み立てられるようにした。moq-pub のテストにあった同等のヘルパは削除して公開 API に寄せた。
- 公開 API になった項目に doc コメントを付け、private 項目への rustdoc リンクを外した。`pipeline::run` の doc は引数と挙動をすべて説明するようにした。
- `cargo build -p moq-pub -p moq-sub`、`cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、`RUSTDOCFLAGS="-D warnings" cargo doc -p moq-pub -p moq-sub --no-deps`、`prek run --all-files` が通ることを確認した。
- 実 relay に対し、lib 化後のバイナリで publisher と subscriber を起動して SETUP / PUBLISH / SUBSCRIBE が成立することを確認した。

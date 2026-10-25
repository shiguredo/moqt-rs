# peer 由来の Reason Phrase をログ出力前にサニタイズする

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §15.9 (Logging of Untrusted String Fields) は、Reason Phrase と MOQT_IMPLEMENTATION が sender
制御のテキストであり、ログや描画の前にサニタイズすることを SHOULD とする ("an endpoint that logs or renders them SHOULD sanitize them first (for example, by
escaping bytes outside the printable ASCII range), since unsanitized values can enable log injection or terminal escape sequence
injection.")。examples は peer 由来の文字列をそのまま tracing に渡しており、端末エスケープシーケンス注入の経路になっている。

## 現状

- `src/message.rs` の `ReasonPhrase` は decode 時に UTF-8 と 1024 バイト上限のみを検証し、制御文字を含む文字列を保持する。
- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` は `Session closed: {:#x} {}` のように peer 由来の
  `SessionError::reason` をそのまま tracing に渡す。
- `examples/tokio-moq/src/moqt_client.rs` は SETUP 交換時の終了理由や REQUEST_ERROR の理由をそのままログへ出す箇所がある。
- `src/parameter.rs` の `SetupOptions` は MOQT_IMPLEMENTATION のバイト列を保持するが、取得 API が無いため現状 examples が表示することはない。
- サニタイズを行う helper はライブラリにも examples にも存在しない。

## 設計方針

- `examples/tokio-moq/src/lib.rs` に `sanitize_for_log(&str) -> String` を追加する。printable ASCII (0x20-0x7E) とタブ以外の文字を `\xNN` /
  `\u{...}` 形式へエスケープし、ANSI エスケープシーケンスを無効化する。
- examples のうち peer 由来の文字列をログに出す箇所へ適用する。対象は session close の理由、REQUEST_ERROR / PUBLISH_DONE の理由、Request Stream GOAWAY の URI
  など、peer が内容を制御できる文字列とする。
- ライブラリの `ReasonPhrase` にログ用メソッドは追加しない。ログ形式はアプリの関心事であり、ライブラリは値をそのまま提供する責務に留める。
- `examples/tokio-moq` の unit test で、制御文字・ESC・非 ASCII を含む文字列がエスケープされ、通常の ASCII はそのまま残ることを固定する。

## 完了条件

- peer 由来の Reason Phrase をログに出す examples の経路で、制御文字と ESC がそのまま出力されないこと。
- 通常の ASCII 文字列は変更されずに出力されること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

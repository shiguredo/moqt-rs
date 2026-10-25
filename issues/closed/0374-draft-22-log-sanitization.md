# peer 由来の Reason Phrase をログ出力前にサニタイズする

- Created: 2026-10-25
- Completed: 2026-10-25
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §15.9 (Logging of Untrusted String Fields) は、Reason Phrase と MOQT_IMPLEMENTATION が sender
制御のテキストであり、ログや描画の前にサニタイズすることを SHOULD とする ("an endpoint that logs or renders them SHOULD sanitize them first (for example, by
escaping bytes outside the printable ASCII range), since unsanitized values can enable log injection or terminal escape sequence
injection.")。examples は peer 由来の文字列をそのまま tracing に渡しており、端末エスケープシーケンス注入の経路になっている。

## 現状

- `src/message.rs` の `ReasonPhrase` は decode 時に UTF-8 と 1024 バイト上限のみを検証し、制御文字を含む文字列を保持する。
- `SessionError::reason` はライブラリが生成する `&'static str` であり peer 由来ではない。peer が内容を制御できる文字列は
  `RequestErrorReceived.reason` (REQUEST_ERROR の `ReasonPhrase`)、`PublishDoneReceived.reason`、`GoawayReceived.new_session_uri`、
  `WtEvent::SessionClosed { reason }` である。
- `examples/tokio-moq/src/moqt_client.rs` は `RequestErrorReceived.reason` を `request_errors` に保持し、
  `TransportError::Internal` の文字列へ埋め込む。この文字列は moq-pub / moq-sub のログにそのまま出る。
- `examples/moq-pub/src/pipeline.rs` と `examples/moq-sub/src/pipeline.rs` は REQUEST_ERROR の理由をログに出す。
  PUBLISH_DONE の理由は現状どちらの example も `..` で捨てている。
- `examples/tokio-moq/src/webtransport_h2.rs` は `WtEvent::SessionClosed { reason }` を `{reason:?}` (Debug 表示) で
  ログに渡しており、制御文字はエスケープされる。ただし `examples/tokio-moq/src/lib.rs` の移行処理は GOAWAY の
  `new_session_uri` を `invalid new session URI '{uri}'` のようにそのまま文字列へ埋め込んでエラーにしている。
- `src/parameter.rs` の `SetupOptions` は MOQT_IMPLEMENTATION のバイト列を保持するが、取得 API が無いため現状 examples が表示することはない。
- サニタイズを行う helper はライブラリにも examples にも存在しない。

## 設計方針

- `examples/tokio-moq/src/lib.rs` に `sanitize_for_log(&str) -> String` を追加する。printable ASCII (0x20-0x7E) 以外の文字を
  `\xNN` / `\u{...}` 形式へエスケープし、ANSI エスケープシーケンスを無効化する。タブは printable ASCII ではないため
  エスケープ対象とする (§15.9 の例も printable ASCII 範囲外のエスケープを挙げる)。
- peer 由来の文字列は、ログ呼び出し地点ではなく文字列を構築する時点でサニタイズする。対象は
  `examples/tokio-moq/src/moqt_client.rs` の `request_errors` に保存する REQUEST_ERROR の理由と、
  `examples/tokio-moq/src/lib.rs` の移行処理が GOAWAY の `new_session_uri` を埋め込むエラー文字列とする。
  `new_session_uri` 自体 (`Vec<u8>`) は移行先 URI として使う値でありサニタイズしない (ログ用の文字列だけを変換する)。
  `examples/tokio-moq/src/webtransport_h2.rs` の `WtEvent::SessionClosed.reason` は既に Debug 表示でエスケープされて
  いるため対象外とする。PUBLISH_DONE の理由は現状ログに出しておらず、本 issue では新たにログへ出さない。
- ライブラリの `ReasonPhrase` にログ用メソッドは追加しない。ログ形式はアプリの関心事であり、ライブラリは値をそのまま提供する責務に留める。
- `examples/tokio-moq` の unit test で、制御文字・ESC・タブ・非 ASCII を含む文字列がエスケープされ、通常の ASCII はそのまま残ることを固定する。
  あわせて REQUEST_ERROR の理由と GOAWAY URI のエラー文字列の構築時にサニタイズされることを固定する。

## 完了条件

- peer 由来の Reason Phrase をログに出す examples の経路で、制御文字と ESC がそのまま出力されないこと。
- 通常の ASCII 文字列は変更されずに出力されること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `examples/tokio-moq/src/lib.rs` に `sanitize_for_log(&str) -> String` を追加した。printable ASCII (0x20-0x7E) 以外を
  `\xNN` (<= 0xFF) / `\u{...}` へエスケープする。タブも printable ASCII ではないためエスケープ対象とした。
- `examples/tokio-moq/src/moqt_client.rs` は REQUEST_ERROR の拒否理由を `TransportError::Internal` の文字列へ埋め込む
  時点で `sanitize_for_log` を適用するようにした。
- `examples/tokio-moq/src/lib.rs` の移行処理は GOAWAY の `new_session_uri` を埋め込むエラー文字列の URI 部分だけを
  `sanitize_for_log` でサニタイズした (`new_session_uri` 自体は移行先 URI として使う値のため変更しない)。
- `examples/tokio-moq/src/webtransport_h2.rs` の `WtEvent::SessionClosed.reason` は既に `{reason:?}` の Debug 表示で
  エスケープされているため変更していない。PUBLISH_DONE の理由は現状ログに出しておらず、新たにログへは出さない。
- `examples/tokio-moq/src/lib.rs` の unit test に、通常の ASCII がそのまま残り、ESC / タブ / DEL / Latin-1 / 非 ASCII が
  エスケープされることを固定するテストを追加した。

## 完了条件の検証

- REQUEST_ERROR の理由と GOAWAY URI のエラー文字列の構築時にサニタイズされること: 実装と
  `sanitize_for_log_escapes_untrusted_characters` で固定した。
- 通常の ASCII 文字列が変更されないこと: 同テストの先頭ケースで固定した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` 成功

# 未知のエラーコードを文脈ごとの INTERNAL_ERROR 相当に正規化する

- Created: 2026-10-25
- Completed: 2026-10-25
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §13 (Grease) は "Receipt of an unknown error code in any error context (Session Termination,
REQUEST_ERROR, PUBLISH_DONE, or Data Stream Reset) MUST be treated as equivalent to INTERNAL_ERROR for that context." と MUST
を課す。現状のライブラリは受信したエラーコードを生の `u64` のままアプリへ公開し、文脈ごとの INTERNAL_ERROR へ正規化する手段を提供していないため、利用者はこの MUST を自力で実装する必要がある。

## 現状

- `src/session/types.rs` の `RequestErrorReceived` / `PublishDoneReceived` は、受信したエラーコードを `u64` のまま保持する。
  受信した Stream Reset は `TerminationReason::PeerStreamReset { error_code: Option<u64> }` と
  `Fetch::peer_stop_sending_error_code: Option<Option<u64>>` が運ぶ (`Option` の `None` は WebTransport でアプリケーション
  エラーコードが無い場合)。`SessionEvent::ResetDataStream` は自側が I/O 層へ出す送信指示であり、受信コードではない。
- ライブラリは peer の Session Termination コードをアプリへ公開しない。peer の接続終了コードは examples の transport 層が
  観測する (`SessionError.code` は自側が閉じるときの値)。
- `src/error.rs` は各レジストリの定数と `is_local_error_code` を持ち、送信経路 (`Session::close` / `send_*`) がローカル専用コードを
  `*_INTERNAL_ERROR` へ置換している。未知コードを文脈別の INTERNAL_ERROR に写す公開の分類 helper は持たない。
- examples は受信コードを既知の定数と比較する箇所と、そのままログへ出す箇所が混在しており、未知コードを INTERNAL_ERROR 相当として扱う経路が統一されていない。
- `docs/missing-features.md` の「要確認」に分類 helper をライブラリへ置くかの検討が残っている。

## 設計方針

- `src/error.rs` に文脈別の写像関数を 4 つ追加する。
  - `normalize_session_termination_code(code: u64) -> u64`
  - `normalize_request_error_code(code: u64) -> u64`
  - `normalize_publish_done_code(code: u64) -> u64`
  - `normalize_stream_reset_code(code: u64) -> u64`
  各関数は文脈のレジストリに属する既知コードをそのまま返し、未知コードはその文脈の `*_INTERNAL_ERROR` を返す。
  Stream Reset は `Option<u64>` の `None` (アプリケーションエラーコード無し) を写像しない (呼び出し側が `None` をそのまま扱う)。
  Session Termination の関数は examples の transport 層が観測した接続終了コードをアプリが渡す想定であり、ライブラリ内部の
  受信経路は無い。目的の MUST を満たすのに必要な最小 API とする。
- 未知コードでセッションを閉じない既存動作は変えない。REQUEST_ERROR / PUBLISH_DONE の未知コード受信をセッション終了にしないことをテストで固定する。
- examples で受信コードの意味によって分岐・記録する箇所は、REQUEST_ERROR / PUBLISH_DONE のコード比較とログ出力
  (`examples/tokio-moq/src/moqt_client.rs` の `request_errors`、`examples/moq-sub/src/pipeline.rs` の受信コード表示) を対象に
  写像関数を使う。wire 上のコードをそのままログに出す箇所は、生のコードと正規化後のコードの両方を出して観測性を落とさない。
- `docs/missing-features.md` の「要確認」から、この分類 helper の項目を実装済みとして整理する。
- `tests/test_error.rs` に 4 文脈の既知コード / 未知コード / GREASE 値を含む写像のテストを追加する。

## 完了条件

- 4 文脈それぞれで、既知コードは同じ値、未知コードは各 `*_INTERNAL_ERROR` を返す写像関数が公開されること。
- 未知の REQUEST_ERROR / PUBLISH_DONE コードを受信してもセッションが閉じないこと。
- `docs/missing-features.md` の該当項目が実装状況に合わせて更新されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/error.rs` に文脈別の公開写像関数を追加した。
  - `normalize_session_termination_code`
  - `normalize_request_error_code`
  - `normalize_publish_done_code`
  - `normalize_stream_reset_code`
  各関数は draft-ietf-moq-transport-22 §16.11 の各レジストリに属する既知コードをそのまま返し、未知コードは
  その文脈の `*_INTERNAL_ERROR` を返す。Stream Reset の `Option<u64>` の `None` (アプリケーションエラーコード
  無し) は写像しない。
- `tests/test_error.rs` に 4 文脈の既知コード / 未知コード / GREASE 値の写像テストを追加した
  (`unknown_error_code_normalization`)。GREASE は `shiguredo_moqt::grease::generate` で生成し、`is_grease` で
  確認している。
- `examples/tokio-moq/src/error.rs` から写像関数を再公開し、`examples/tokio-moq/src/moqt_client.rs` が
  REQUEST_ERROR の拒否理由を組み立てるときに生コードと正規化コードの両方を出すようにした。
  `examples/moq-sub/src/pipeline.rs` は PUBLISH_DONE のログに生コードと正規化コードの両方を出すようにした。
- `docs/missing-features.md` の「要確認」から、分類 helper をライブラリに置くかの検討項目を削除した
  (実装済みのため)。

## 完了条件の検証

- 4 文脈の既知 / 未知 / GREASE の写像: `unknown_error_code_normalization::known_codes_are_unchanged` /
  `unknown_codes_are_normalized_to_internal_error` / `grease_codes_are_normalized_to_internal_error`
- 未知の REQUEST_ERROR / PUBLISH_DONE コードを受信してもセッションが閉じないことは既存の受信テスト
  (`tests/test_session/` の REQUEST_ERROR / PUBLISH_DONE 受信テスト) が維持している。
- `docs/missing-features.md` の該当項目を削除した。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` 成功

# 未知のエラーコードを文脈ごとの INTERNAL_ERROR 相当に正規化する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §13 (Grease) は "Receipt of an unknown error code in any error context (Session Termination,
REQUEST_ERROR, PUBLISH_DONE, or Data Stream Reset) MUST be treated as equivalent to INTERNAL_ERROR for that context." と MUST
を課す。現状のライブラリは受信したエラーコードを生の `u64` のままアプリへ公開し、文脈ごとの INTERNAL_ERROR へ正規化する手段を提供していないため、利用者はこの MUST を自力で実装する必要がある。

## 現状

- `src/session/types.rs` の `RequestErrorReceived` / `PublishDoneReceived` / `ResetDataStream` などのイベントは、受信したエラーコードを `u64` のまま保持する。
- `src/error.rs` は各レジストリの定数と、ローカル専用コードを `*_INTERNAL_ERROR` へ置換する内部処理を持つが、未知コードを文脈別の INTERNAL_ERROR に写す公開の分類 helper を持たない。
- examples は受信コードを既知の定数と比較する箇所と、そのままログへ出す箇所が混在しており、未知コードを INTERNAL_ERROR 相当として扱う経路が統一されていない。
- `docs/missing-features.md` の「要確認」に分類 helper をライブラリへ置くかの検討が残っている。

## 設計方針

- `src/error.rs` に文脈別の写像関数を追加する。対象は Session Termination / REQUEST_ERROR / PUBLISH_DONE / Stream Reset の 4
  文脈で、既知コードはそのまま返し、未知コードは各レジストリの `*_INTERNAL_ERROR` を返す。
- 未知コードでセッションを閉じない既存動作は変えない。REQUEST_ERROR / PUBLISH_DONE の未知コード受信をセッション終了にしないことをテストで固定する。
- examples で受信コードの意味によって分岐・記録する箇所は、必要に応じて写像関数を使う。wire 上のコードをそのままログに出す箇所は、生のコードと正規化後のコードの両方を出して観測性を落とさない。
- `docs/missing-features.md` の「要確認」から、この分類 helper の項目を実装済みとして整理する。
- `tests/test_error.rs` に 4 文脈の既知コード / 未知コード / GREASE 値を含む写像のテストを追加する。

## 完了条件

- 4 文脈それぞれで、既知コードは同じ値、未知コードは各 `*_INTERNAL_ERROR` を返す写像関数が公開されること。
- 未知の REQUEST_ERROR / PUBLISH_DONE コードを受信してもセッションが閉じないこと。
- `docs/missing-features.md` の該当項目が実装状況に合わせて更新されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

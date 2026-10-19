# FETCH ストリームの malformed track 分類と End of Range 後続検証を仕様に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-stream-validation
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §12.1 (Malformed Tracks) と §11.4.1.2 (End of Range) に照らし、FETCH 応答ストリームの検証に 2 つの不備がある。1 つは同一 Group の Object ID 非増加と Group 順序違反が `MessageError::ProtocolViolation` で返るのにメッセージは "malformed track" であり、同じ関数内の Priority 変更・最終 Object 超過の `MalformedTrack`
と分類が食い違うこと。もう 1 つは End of Range が宣言した範囲の内側に後続 Object が来ても検出しないこと。

## 現状

- `src/stream/decoder.rs` の `validate_fetch_object` は、同一 Group の Object ID 非増加と Group 順序違反を `ProtocolViolation` で返し、Priority 変更と最終 Object 超過は `MalformedTrack` で返す。
- `tests/test_stream/decoder.rs` は Group 順序違反に `ProtocolViolation` を期待しており、分類の誤りを固定している。
- End of Range を処理した後も `validation.last_object` (最後の実 Object) としか比較しないため、Ascending の実 Object (10, 5) → EOR (10, 20) → 続く Object (10, 15) を受理する。§11.4.1.2 は「last serialized Object から EOR Location まで inclusive の Location」を宣言範囲とする。
- 影響: 分類を型で分岐する利用者が、購読 cancel で足りる違反をセッション終了として扱う。EOR 範囲内の不正 Object を見逃す。

## 設計方針

- 同一 Group の Object ID 非増加・Group 順序違反を `MalformedTrack` に統一する。テストの期待値も更新する。
- 最後の EOR Location を保持し、後続 Object が範囲内なら `MalformedTrack` として拒否する。Ascending / Descending を区別する。
- 同一 Group で Object ID が減少するケースと EOR 範囲内のケースのテストを追加する。

## 完了条件

- `validate_fetch_object` の違反分類が §12.1 の malformed track に統一されていること。
- EOR 範囲内の後続 Object を拒否すること。
- 上記を `tests/test_stream` と PBT で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

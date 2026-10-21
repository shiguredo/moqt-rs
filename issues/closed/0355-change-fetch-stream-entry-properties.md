# FETCH 応答 Object の Properties の扱いを明確にする

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/change-fetch-stream-entry-properties
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §11.4.3 (Fetch Object) の Object は Properties を持ち得るが、`FetchStreamEntry` は decode 時に Properties を黙って捨てる。フィルタ評価にだけ使って利用者へ渡さない設計なら、その契約を doc に明記する。利用者へ渡す設計なら accessor を追加する。黙って捨てる現状は、利用者が Properties の存在を認識できない。

## 現状

- `src/stream/fetch.rs` の `FetchStreamEntry::decode` は Properties を消費して `properties_bytes` として保持するが、公開 accessor が無い。
- `src/session/fetch.rs` は応答の受信時に Properties を購読フィルタの評価に使うが、アプリへ渡す経路が無い。
- テストは Properties の有無を検証していない。
- 影響: アプリが Object 単位のメタデータ (Priority / Expires / Delivery Timeout など) を取得できない。

## 設計方針

- Properties をアプリへ渡す場合は、`FetchStreamEntry` に `properties()` を追加し、session の受信イベントにも載せる。
- 渡さない場合は、decode の doc とモジュール doc に「Properties はフィルタ評価にのみ使い、利用者へは渡さない」と明記する。
- どちらを採るかを決めてテストで固定する。

## 完了条件

- Properties の扱いが doc と実装で一致していること。
- 非空 Properties を持つ FETCH Object のテストがあること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

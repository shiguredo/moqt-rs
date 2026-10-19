# CoseMessage::encode で alg とメッセージ種別・タグの整合を検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-c4m-cose-encode-validation
- Polished: {YYYY-MM-DD}

## 目的

`src/c4m/cose.rs` の `CoseMessage::encode` は保護ヘッダの `alg` と Sign1 / Mac0 の種別、COSE タグの整合を検証しない。公開フィールドから HMAC alg を持つ `CoseSign1` を構築して encode すると、自実装の decode が `AlgorithmClassMismatch` で拒否するメッセージを生成できる。`CatTokenBuilder` 経由では安全だが、公開 API の非対称を解消する。

## 現状

- `CoseMessage::encode` は protected header の bytes をそのまま書き、`CoseSign1` / `CoseMac0` の種別と `alg` の対応を検証しない。
- decode は `AlgorithmClassMismatch` で種別と alg の不整合を拒否する。
- 公開フィールドで任意の `CoseSign1` / `CoseMac0` を構築できる。
- 影響: encode と decode で受理範囲が非対称。利用者が decode 不能な COSE メッセージを生成し得る。

## 設計方針

- encode で `header()` の `alg` が `CoseSign1` なら署名 alg、`CoseMac0` なら MAC alg であることを検証する。タグも種別に合わせる。
- あるいは `CoseSign1` / `CoseMac0` の構築を検証付きコンストラクタに限定する (破壊的変更として扱う)。
- 不整合の encode テストを追加する。

## 完了条件

- 不整合な COSE メッセージの encode がエラーになること。
- 正当な Sign1 / Mac0 の roundtrip が維持されること。
- `cargo test --workspace` / `cargo test --workspace --features aws-lc-rs` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

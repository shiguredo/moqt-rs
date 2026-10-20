# CoseMessage::encode で alg とメッセージ種別の整合を検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-c4m-cose-encode-validation
- Polished: 2026-10-21

## 目的

`src/c4m/cose.rs` の `CoseMessage::encode` は protected ヘッダの `alg` と Sign1 / Mac0 の種別の対応を検証しない。公開フィールドから HMAC alg を持つ `CoseSign1` を構築し、既定の `CoseEncodingOptions` (COSE タグと CWT タグを付与する) で encode すると、自実装の decode が `AlgorithmClassMismatch` で拒否するメッセージを生成できる。`CatTokenBuilder` 経由では安全だが、公開 API の非対称を解消する。

COSE タグはメッセージ種別から `AlgorithmClass::tag()` で決まるため、タグと種別の整合は encode で既に保証されている。検証すべきは `alg` の class とメッセージ種別の対応である。

## 現状

- `CoseMessage::encode` は protected / unprotected ヘッダをそのまま書き、`CoseSign1` / `CoseMac0` の種別と `alg` の対応を検証しない。
- `CoseMessage::decode` はタグ付きメッセージについて、`CoseSign1` に MAC alg、`CoseMac0` に署名 alg が付いていると `AlgorithmClassMismatch` を返す。
- `CoseMessage::decode` はタグ無しメッセージを `alg` から Sign1 / Mac0 に分類する。`alg` が無い場合は `MissingAlgorithm` を返す。`alg` の class が encode した variant と食い違う場合、decode は `alg` に従って別の variant を返す。
- `CoseMessage::decode` はタグ付きメッセージで `alg` が無い場合、タグから種別を決めて受理する。
- `CoseMessage::encode` は COSE タグをメッセージ種別から `AlgorithmClass::tag()` で決めるため、タグと種別の整合は常に保証されている。CWT タグには COSE タグが必要という検証は既にある。
- `CoseSign1` / `CoseMac0` は公開フィールドを持ち、利用者が任意の protected header を持つメッセージを構築できる。
- 影響: `CoseMessage::encode` と `CoseMessage::decode` で受理範囲が非対称になる。利用者が decode 不能な COSE メッセージ、または decode で別の種別に解釈される COSE メッセージを生成し得る。

## 再現手順

1. `Header { algorithm: Some(Algorithm::HmacSha256), algorithm_identifier: Some(Algorithm::HmacSha256.identifier()), ..Default::default() }` を `Header::encode` で `Value` にし、`cbor::encode` でバイト列にする。
2. そのバイト列を `protected` に持つ `CoseSign1 { unprotected: Vec::new(), payload: Some(b"payload".to_vec()), signature: vec![0x01], cose_tagged: true, cwt_tagged: true }` を作る。
3. `CoseMessage::Sign1(...)` を `CoseEncodingOptions::default()` で `encode` する。現状はエラーにならず、CWT タグ 61 + COSE タグ 18 の CBOR が返る。
4. 返ったバイト列を `CoseMessage::decode` に渡すと `Err(CoseError::AlgorithmClassMismatch)` になる。

## 設計方針

- `CoseMessage::encode` の先頭で `self.header()?` を呼び、protected / unprotected ヘッダを decode 時と同じ規則で検証する。これにより、未知の alg、unprotected の alg、重複ラベル、不正な crit など、decode が拒否するヘッダを encode でも拒否する。`header()` は C4M ドラフトの別名 `-4` を `HmacSha256` に解決するため、class 判定は解決後の `Algorithm` を使う。
- `header().algorithm` が `Some(algorithm)` の場合、メッセージ種別とアルゴリズム class を照合する。
  - `CoseMessage::Sign1` は `AlgorithmClass::Signature` を要求する。
  - `CoseMessage::Mac0` は `AlgorithmClass::Mac` を要求する。
  - 不一致なら `CoseError::AlgorithmClassMismatch` を返す。
- `header().algorithm` が `None` の場合、decode の受理範囲に合わせる。
  - タグ付きメッセージ (encode 内で計算する `tagged` が true) は decode がタグから種別を決めて受理するため、encode も受理する。
  - タグ無しメッセージ (encode 内で計算する `tagged` が false) は decode が `MissingAlgorithm` で拒否するため、encode も `MissingAlgorithm` を返す。
- COSE タグはメッセージ種別から `AlgorithmClass::tag()` で決まるため、個別のタグ検証は追加しない。CWT タグには COSE タグが必要という既存の検証は維持する。
- `CoseSign1` / `CoseMac0` の公開フィールドは変更しない。構築を検証付きコンストラクタに限定する案は、公開フィールドを private にする破壊的変更になるため採用しない。encode の境界で検証する。
- `CoseMessage::encode` の doc コメントに、検証内容と返すエラーを追記する。

## 変更対象

- `src/c4m/cose.rs`
- `tests/test_c4m/cose.rs`

## 完了条件

- `CoseSign1` に MAC alg、`CoseMac0` に署名 alg を設定したメッセージの encode が、タグ付き / タグ無しのどちらでも `CoseError::AlgorithmClassMismatch` を返すこと。
- `alg` が無いタグ無しメッセージの encode が `CoseError::MissingAlgorithm` を返すこと。
- unprotected にしか `alg` が無いメッセージの encode が `CoseError::UnprotectedAlgorithm` を返すこと。
- `alg` が無いタグ付きメッセージの encode が成功し、decode で同じ種別に戻ること。
- 正当な Sign1 / Mac0 の roundtrip が維持されること (既存の `tests/test_c4m/cose.rs` と `pbt/tests/prop_c4m/cose.rs` で確認する)。
- `tests/test_c4m/cose.rs` に上記の encode エラーとタグ付き / タグ無しの受理範囲を固定するテストが追加されていること。
- `CoseMessage::encode` の doc コメントが、`header()` の検証 (例: `UnprotectedAlgorithm`)、`AlgorithmClassMismatch`、`MissingAlgorithm` を説明していること。
- 次のコマンドが通ること。
  - `cargo test --workspace`
  - `cargo test -p shiguredo_moqt --features aws-lc-rs`
  - `cargo clippy --workspace --all-targets -- -D warnings`
  - `cargo clippy -p shiguredo_moqt --all-targets --features aws-lc-rs -- -D warnings`
  - `cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt`
  - `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt --features aws-lc-rs`
  - `cargo fmt --all -- --check`

## 対象外

- `CoseSign1` / `CoseMac0` の公開フィールドを private にし、検証付きコンストラクタだけに構築を限定する変更。本 issue は encode の境界で検証する。
- `CoseMessage::decode` のタグ付きメッセージで `alg` が無い場合の受理 (既存挙動)。本 issue は encode の受理範囲を decode に合わせるだけで、decode は変更しない。

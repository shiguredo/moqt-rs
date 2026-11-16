# MSF カタログの cipherSuite 必須条件を moq-secure-objects に限定する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/change-msf-cipher-suite-condition
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.39 (Cipher suite) は、`cipherSuite` を `encryptionScheme` が `"moq-secure-objects"` のときだけ必須とした (MUST)。v01 は `encryptionScheme` が指定されていれば常に必須だった。
現行実装は v01 の条件で検証しており、v02 で許される独自の暗号化方式を、`cipherSuite` なしという理由で誤って拒否する。

## 現状

- `src/msf.rs` の `validate_full_track`、`decode_track`、`MsfCloneTrack::into_track` は、`encryption_scheme.is_some()` なら `cipher_suite` を必須にする。
- `tests/test_msf/encode_decode_roundtrip.rs` の `encryption_cipher_suite_missing_rejected` が、方式に依存せず拒否する挙動を固定している。
- `MsfTrack.encryption_scheme` / `MsfTrack.cipher_suite` は `Option<String>` で、`cipherSuite` の値の検証は行っていない。`encryptionScheme` が `moq-secure-objects` のときは `keyId` と `trackBaseKey` も必須にしている。
- v02 §5.2.39 は `cipherSuite` を `encryptionScheme` が `moq-secure-objects` のときにのみ要求する。§5.2.38 は `moq-secure-objects` を RECOMMENDED とする。

## 設計方針

- `cipherSuite` の必須判定を `encryption_scheme == Some("moq-secure-objects")` に限定する。
- `cipherSuite` の値の検証は追加しない。仕様は `moq-secure-objects` の定義値を列挙するが、未知値の拒否を MUST としていないため、値の集合を強制しない。`moq-secure-objects` のときの `keyId` / `trackBaseKey` 必須検証は維持する。
- `encryptionScheme` がない場合に `cipherSuite` だけがある状態の扱い (拒否するか無視するか) を仕様に合わせて決め、doc とテストで固定する。v02 は無暗号化を既定とするため、`cipherSuite` 単独は受理しない方針とする。
- 既存の `encryption_cipher_suite_missing_rejected` を、`moq-secure-objects` のときだけ拒否するテストに置き換える。

## 完了条件

- `"encryptionScheme": "moq-secure-objects"` で `cipherSuite` がないカタログが `MessageError::InvalidCatalog` で拒否されること。
- `"encryptionScheme": "custom-scheme"` で `cipherSuite` がないカタログが受理されること。
- `"encryptionScheme": "moq-secure-objects"` で `keyId` または `trackBaseKey` がないカタログが拒否されること。
- `cipherSuite` だけを持ち `encryptionScheme` を持たないカタログの扱いがテストで固定されていること。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

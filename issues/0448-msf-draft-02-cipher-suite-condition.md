# MSF カタログの cipherSuite 必須条件を moq-secure-objects に限定する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.39 (Cipher suite) は、`cipherSuite` を `encryptionScheme` が `"moq-secure-objects"` のときだけ必須とした (MUST)。v01 は `encryptionScheme` が指定されていれば常に必須だった。

現行実装は v01 の条件で検証しており、v02 で許される独自の暗号化方式を、`cipherSuite` なしという理由で誤って拒否する。

## 現状

- `src/msf.rs` の `validate_full_track`、`decode_track`、`MsfCloneTrack::into_track` の 3 箇所が、`encryption_scheme.is_some()` なら `cipher_suite` を必須にする。
- 同じ 3 箇所で、`encryptionScheme` が `"moq-secure-objects"` のときは `keyId` と `trackBaseKey` も必須にしている。この条件は v02 でも維持する。
- `tests/test_msf/encode_decode_roundtrip.rs` の `encryption_cipher_suite_missing_rejected` が、方式に依存せず拒否する挙動を固定している。
- `tests/test_msf/encode_decode_roundtrip.rs` の `encryption_orphan_field_accepted` は、`encryptionScheme` なしで `cipherSuite` だけを持つカタログの受理を固定している。v02 §5.2.38 は `encryptionScheme` の欠如を「unencrypted」とするだけで、`cipherSuite` 単独を禁止しない。
- `tests/test_msf/error_cases.rs` の `encode_full_encryption_scheme_missing_cipher_suite_rejected` が、encode 側で同じ旧挙動を固定している。
- `cipherSuite` の値が §5.2.39 の定義値であることの検証は、open の 0342 が担当する。§4.4.3 も `moq-secure-objects` のときの supported cipher suite を MUST とする。

## 設計方針

- `cipherSuite` の必須判定を `encryption_scheme == Some("moq-secure-objects")` に限定する。
- `keyId` / `trackBaseKey` の必須判定は、`moq-secure-objects` のときという現行条件を維持する。
- `encryptionScheme` がない場合の `cipherSuite` 単独は、仕様が禁止していないため受理する (無視する)。`encryption_orphan_field_accepted` を維持する。
- `cipherSuite` の値の検証は本 issue では追加しない。0342 に委譲し、その旨を issue に明記する。
- `tests/test_msf/encode_decode_roundtrip.rs` の `encryption_cipher_suite_missing_rejected` を、`moq-secure-objects` のときだけ拒否するテストに置き換える。`tests/test_msf/error_cases.rs` の encode 側テストも同じ方針に置き換える。

## 完了条件

- `"encryptionScheme": "moq-secure-objects"` で `cipherSuite` がないカタログが、decode と encode の両方で `MessageError::InvalidCatalog` になること。
- `"encryptionScheme": "custom-scheme"` で `cipherSuite` がないカタログが受理されること。
- `"encryptionScheme": "moq-secure-objects"` で `keyId` または `trackBaseKey` がないカタログが拒否されること。
- `encryptionScheme` なしで `cipherSuite` だけを持つカタログが受理されること (`encryption_orphan_field_accepted` を維持)。
- `cipherSuite` の値の検証は 0342 が担当することが issue に明記されていること。
- 上記を `tests/test_msf/catalog_decode.rs`、`tests/test_msf/encode_decode_roundtrip.rs`、`tests/test_msf/error_cases.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

# MSF カタログの connectionUri を connectionURI に改名する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.37 (Connection URI) は、JSON key を `connectionURI` と定める。§5.2 のフィールド表と §5.6.16 の例も `connectionURI` である。
現行実装は `connectionUri` で encode / decode しており、v02 の publisher が出力する `connectionURI` を読めず、逆に v02 の subscriber には `connectionUri` を出力してしまう。

## 現状

- `src/msf.rs` の `write_track_json` は `"connectionUri"` を出力し、`decode_common_track_fields` は `"connectionUri"` を読む。
- 公開フィールドは `MsfTrack.connection_uri` / `MsfCloneTrack.connection_uri` で、JSON key だけが v01 のままである。
- `tests/test_msf` と `crates/moq-pub` / `crates/moq-sub` のカタログ生成・解釈は `connectionUri` を前提にしている。
- v02 §5.2.37 は値が [MOQT] §6.1 の valid な MOQT endpoint URI であることを MUST とする。値の検証は既存 issue で扱う。
- 既存 issue の `connectionUri` の検証は、キー名の変更後に前提を更新する必要がある。

## 設計方針

- JSON key を `connectionURI` に変更する。decode は `connectionURI` だけを読み、encode も `connectionURI` だけを出力する。
- 公開フィールド名 `connection_uri` は Rust の命名として維持し、JSON key の対応だけを変える。
- `connectionUri` (旧 key) は未知フィールドとして無視される。互換のための別名受理は行わない。
- `crates/moq-pub` / `crates/moq-sub` とテストの `connectionUri` を `connectionURI` に更新する。

## 完了条件

- `"connectionURI": "moqt://example.com:4443"` を持つカタログが decode され、`MsfTrack.connection_uri` に値が入ること。
- encode したカタログに `"connectionURI"` が含まれ、`"connectionUri"` が含まれないこと。
- `"connectionUri"` だけを持つカタログでは `connection_uri` が `None` になること (未知フィールドとして無視)。
- 上記を `tests/test_msf/catalog_decode.rs` と `tests/test_msf/encode_decode_roundtrip.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

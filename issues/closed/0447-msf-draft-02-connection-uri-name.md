# MSF カタログの connectionUri を connectionURI に改名する

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §5.2.37 (Connection URI) は、JSON key を `connectionURI` と定める。§5.2 の Table 4 と §5.6.17 (A catalog referencing other catalogs) の例も `connectionURI` である。v01 の §5.6.16 は v02 で §5.6.17 に移動した。

現行実装は `connectionUri` で encode / decode しており、v02 の publisher が出力する `connectionURI` を読めず、逆に v02 の subscriber には `connectionUri` を出力してしまう。

## 現状

- `src/msf.rs` の `write_track_json` は `"connectionUri"` を出力し、`decode_common_track_fields` は `"connectionUri"` を読む。
- 公開フィールドは `MsfTrack.connection_uri` / `MsfCloneTrack.connection_uri` で、JSON key だけが v01 のままである。
- `connectionUri` の出現は `src` に 2、`tests` に 12 (`tests/test_msf/track_fields.rs` 7、`debug_redaction.rs` 2、`encode_decode_roundtrip.rs` 3)、`docs/msf.md` に 1 ある。`crates/` にはない。
- `tests/test_msf/track_fields.rs` の `connection_uri_token_wrong_type_rejected` は `"connectionUri": 4443` の型エラー拒否を固定している。改名後は `connectionUri` が未知フィールドとして無視されるため、この期待値は成立しない。
- v02 §5.2.37 は値が [MOQT] §6.1 の valid な MOQT endpoint URI であることを MUST とする。値の検証は 0342 が担当し、§5.2.36 (Track duration) ではなく §5.2.37 を参照するように前提を更新する必要がある。

## 設計方針

- JSON key を `connectionURI` に変更する。decode は `connectionURI` だけを読み、encode も `connectionURI` だけを出力する。
- 公開フィールド名 `connection_uri` は Rust の命名として維持し、JSON key の対応だけを変える。
- `connectionUri` (旧 key) は未知フィールドとして無視される。互換のための別名受理は行わない。
- `tests/test_msf/track_fields.rs` の `connectionUri` を使う 7 テストを `connectionURI` に更新する。型エラーのテストは `connectionURI` の型検査として維持する。
- `tests/test_msf/debug_redaction.rs` と `encode_decode_roundtrip.rs` の `connectionUri` を更新し、`pbt/tests/prop_msf.rs` に `connectionUri` の記述があれば合わせる。
- `docs/msf.md` と `docs/missing-features.md` のフィールド名を `connectionURI` に更新する。
- 0342 の完了条件にある `connectionUri` の検証と §5.2.36 の参照は、本 issue の完了後に `connectionURI` と §5.2.37 へ更新する前提とする。

## 完了条件

- `"connectionURI": "moqt://example.com:4443"` を持つカタログが decode され、`MsfTrack.connection_uri` に値が入ること。
- encode したカタログに `"connectionURI"` が含まれ、`"connectionUri"` が含まれないこと。
- `"connectionUri"` だけを持つカタログでは `connection_uri` が `None` になること (未知フィールドとして無視)。`"connectionUri": 4443` のような型違いも拒否せず無視されること。
- `rg 'connectionUri' src tests pbt docs` が 0 件であること。
- 0342 が §5.2.37 と `connectionURI` を参照する前提であることが issue に明記されていること。
- 上記を `tests/test_msf/track_fields.rs`、`tests/test_msf/encode_decode_roundtrip.rs`、`tests/test_msf/debug_redaction.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- MSF カタログの JSON key を `connectionUri` から `connectionURI` に変更した。decode は `connectionURI` だけを読み、encode も `connectionURI` だけを出力する。公開 Rust フィールド名 `connection_uri` は維持した。
- 旧 key は未知フィールドとして無視し、互換のための別名受理は行わない。文字列値だけでなく型違いも無視されることをテストで固定した。
- `tests/test_msf/track_fields.rs` / `debug_redaction.rs` / `encode_decode_roundtrip.rs` の JSON key とコメントを更新し、`pbt/tests/prop_msf.rs` のコメントも追従した。
- `docs/msf.md` と `docs/missing-features.md` の key と節番号を `connectionURI` / §5.2.37 に更新した。0342 の issue も `connectionURI` と §5.2.37 を前提とする記述に更新した。
- `rg 'connectionUri' src tests pbt docs` が 0 件であることを確認した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。

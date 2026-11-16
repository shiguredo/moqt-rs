# MSF カタログから track-level token フィールドを削除する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 は track-level の `token` フィールドを削除した (Appendix A.1.3: 「The track-level token field was removed. Publish tracks now use authInfo.」)。publish の認可は `authInfo` (§5.2.42) に一本化された。

現行実装は削除された `token` を wire に出力し続けており、最新ドラフトに準拠していない。v02 の parser は未知フィールドを無視するため、この出力だけが相互運用を壊すわけではないが、削除済みのフィールドを出し続ける状態と、認可情報の経路が 2 つある状態を解消する。

## 現状

- `MsfTrack.token: Option<String>`、`MsfCloneTrack.token: Option<String>`、内部の `TrackJsonParts.token`、clone 適用時の `CommonTrackFields.token` がある。
- `write_track_json` は `"token"` を出力し、`decode_common_track_fields` は `"token"` を読む。`MsfCloneTrack::into_track` も `token` を継承する。
- `authInfo` は `MsfTrack.auth_info: Option<Vec<MsfAuthInfo>>` として実装済みである。
- v02 §5 のフィールド表に `token` はなく、§5 は parser が理解しないフィールドを無視することを MUST とする。
- `MSF_VERSION` は 0437 まで `"draft-01"` のため、`"version":"draft-02"` の完全カタログは `InvalidCatalogVersion` で拒否される。0437 が入るまでのテストは、`"version":"draft-01"` の完全カタログ、または version を持たない delta update で `token` の無視を固定する。
- `pbt/tests/prop_msf.rs` が `MsfTrack.token` / `MsfCloneTrack.token` と `sample_token_like` を参照しており、フィールドを削除するとコンパイルできない。`docs/msf.md` も `token` を対応フィールドとして列挙している。

## 設計方針

- `MsfTrack` / `MsfCloneTrack` / `TrackJsonParts` / `CommonTrackFields` から `token` を削除する。encode は `token` を出力しない。
- v02 のカタログに `token` が現れた場合は、§5 の未知フィールドの扱いに従って無視する。値の解釈や認可への利用はしない。
- `MsfCloneTrack::into_track` の `token` 継承を削除する。
- `authInfo` による認可の提示は既存の実装を維持する。`authInfo` の値の中に現れる `token` キーと、MSF URI の token パラメータは別物であり、本 issue の対象外とする。
- `pbt/tests/prop_msf.rs` から `token` と `sample_token_like` を削除し、`docs/msf.md` のフィールド一覧を更新する。
- 公開型からのフィールド削除は破壊的変更になる。`CODEBASE.md` の方針に従い、別名や非推奨フィールドは残さない。変更履歴は `CODEBASE.md` の指示により `CHANGES.md` には残さない。

## 完了条件

- encode したカタログに `"token"` が含まれないこと。
- `"version":"draft-01"` の完全カタログ、または version を持たない delta update に `"token"` を含めた場合、decode が成功して `token` の値が無視されること。`authInfo` は従来どおり解釈されること。
- `MsfCloneTrack::into_track` が `token` を継承しないこと。
- `src/msf.rs`、`tests/test_msf`、`pbt/tests/prop_msf.rs`、`docs/msf.md` に track-level `token` を参照するコード・テスト・記述が残っていないこと。
- `pbt/tests/prop_msf.rs` の `sample_token_like` が削除され、`cargo test --workspace` がコンパイルエラーなく通ること。
- `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

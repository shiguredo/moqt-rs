# MSF カタログから track-level token フィールドを削除する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 は track-level の `token` フィールドを削除した (A.1.3: "The track-level token field was removed. Publish tracks now use authInfo.")。publish の認可は `authInfo` (§5.2.42) に一本化された。
現行実装は `token` を wire に出力し続けており、v02 のカタログと相互運用できない。認可情報の経路が 2 つある状態も望ましくない。

## 現状

- `MsfTrack.token: Option<String>`、`MsfCloneTrack.token: Option<String>`、内部の `TrackJsonParts.token`、clone 適用時の `CommonTrackFields.token` がある。
- `write_track_json` は `"token"` を出力し、`decode_common_track_fields` は `"token"` を読む。`MsfCloneTrack::into_track` も `token` を継承する。
- `authInfo` は `MsfTrack.auth_info: Option<Vec<MsfAuthInfo>>` として実装済みで、`MsfAuthInfo.value_raw` は任意の JSON 値を保持する。
- v02 §5.2 のフィールド表に `token` はない。A.1.3 は削除を明記する。

## 設計方針

- `MsfTrack` / `MsfCloneTrack` / `TrackJsonParts` / `CommonTrackFields` から `token` を削除する。encode は `token` を出力しない。
- v02 のカタログに `token` が現れた場合は、§5 の未知フィールドの扱いに従って無視する。値の解釈や認可への利用はしない。この挙動を doc とテストで固定する。
- `authInfo` による認可の提示は既存の実装を維持し、本 issue では `token` の削除と `authInfo` への一本化だけを行う。
- 公開型からのフィールド削除は破壊的変更になる。`CODEBASE.md` の方針に従い、別名や非推奨フィールドは残さない。

## 完了条件

- encode したカタログに `"token"` が含まれないこと。
- `"token"` を含む v02 カタログが decode でき、`token` の値が無視されること。`authInfo` は従来どおり解釈されること。
- `MsfCloneTrack::into_track` が `token` を継承しないこと。
- `src/msf.rs` と `tests/test_msf` に `token` を前提としたコード・テストが残っていないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

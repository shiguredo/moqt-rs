# SubgroupObject::decode の Properties 再デコードとマジックナンバーを整理する

- Created: 2026-09-12
- Completed: 2026-09-17
- Branch: feature/refactor-subgroup-decode-cleanup
- Polished: 2026-09-17

## 目的

`SubgroupObject::decode` に残る到達不能なエラー分岐を削除し、Normal status 判定のマジックナンバーを定数に置き換えて、encode 側と共通の検証に寄せる。

## 現状

- `src/stream/subgroup.rs` の `SubgroupObject::decode` は、`checked_len` で切り出した `properties_bytes` を再度 `varint::decode` し、失敗時に `ProtocolViolation("malformed properties length in subgroup object")` を返す。切り出し時点で「Length varint + ちょうど Length バイト」であることが保証されているため、この `map_err` 分岐は到達不能である。
- 同じ箇所の `matches!(status, Some(s) if s != 0)` の `0` は `src/stream.rs` の `pub(crate) const OBJECT_STATUS_NORMAL` (0x0) と同義だが、リテラルで書かれている。
- encode 側は 0052 で `validate_properties_blob` に統一済みのため、decode 側だけ重複した再デコードが残っている。

## 設計方針

- decode 側の再デコードを `validate_properties_blob` に置き換え、到達不能なエラーメッセージを削除する。
- Normal 判定に `OBJECT_STATUS_NORMAL` を使う。
- 挙動は変えない。

## 完了条件

- 到達不能な `map_err` 分岐が削除されていること
- `OBJECT_STATUS_NORMAL` が使われていること
- 既存テスト (`tests/test_stream/subgroup_object.rs` の非 Normal status 検査等) が通ること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

`src/stream/subgroup.rs` の `SubgroupObject::decode` に残っていた到達不能なエラー分岐を削除し、
Properties Length の読み取りを encode 側と同じ `validate_properties_blob` に置き換えた。
Normal status 判定のリテラル `0` は `OBJECT_STATUS_NORMAL` に置き換えた。挙動と公開 API は変えていない。

- `SubgroupObject::decode` の再デコード: `varint::decode(props).map_err(|_| ProtocolViolation("malformed properties length in subgroup object"))?`
  を `validate_properties_blob(props)?` に置き換え、到達不能なエラーメッセージを削除した。この分岐は
  `checked_len` で「Length varint + ちょうど Length バイト」に切り出したスライスに対して再デコードしており、
  `varint::decode` が途中で切れた入力を返すことはなく `map_err` は到達しない。`validate_properties_blob` は
  `varint::decode` に加えて「宣言 Length と実データ長の一致」も検証するが、切り出し済みのスライスは常に
  この契約を満たすため受理・拒否の結果は従来と同一になる。encode 側と decode 側で Properties Length の
  読み取りが 1 箇所に揃い、重複した再デコードが消えた
- Normal status 判定: `matches!(status, Some(s) if s != 0)` と `matches!(self.status, Some(s) if s != 0)`
  (encode 側) のリテラル `0` を `src/stream.rs` の `OBJECT_STATUS_NORMAL` に置き換えた。`0x0` は同定数の
  値と同義であり判定結果は変わらない。値の意味が定数名で読めるようになり、draft の status 値の変更にも
  1 箇所の修正で追従できる
- 変更は `SubgroupObject` の 2 メソッドに閉じており、公開 API の変更はないため `CHANGES.md` の
  `## develop` の `### misc` に `[UPDATE]` エントリを追加した

検証は `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、
`cargo fmt --all -- --check`、`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` で行い、すべて通った。
非 Normal status への Properties 付与を拒否する既存テスト
(`tests/test_stream/subgroup_object.rs` の `non_normal_status_with_properties_rejected` と
`non_normal_status_with_zero_length_properties_encoded`) は変更なしで通っている。

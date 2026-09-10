# MsfTrack / MsfCloneTrack のエンコード・検証の重複を解消する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-msf-track-clone-duplication

## 目的

`src/msf.rs` の `MsfTrack` と `MsfCloneTrack` のフィールド同期箇所を減らし、フィールド追加時の修正漏れを防ぐ。`src/msf.rs` (約 3,400 行) の保守負債を下げる。

## 現状

`MsfTrack` (41 フィールド) と `MsfCloneTrack` (39 フィールド) が同じ JSON メンバー列を別々に持ち、`DisplayJson`・`decode`・`validate` がほぼ重複している。デコードは `decode_common_track_fields` で一部共通化されているが、`DisplayJson` と検証は共通化されていない。

対象: `src/msf.rs` の `MsfTrack` / `MsfCloneTrack` の `DisplayJson`、`decode_track`、`decode_clone_track`、`validate_full_track`、`validate_clone_track_fragment`、`MsfCloneTrack::into_track`。

## 設計方針

継承可能フィールドを `Option` にした共通フラグメント型を 1 つ用意し、`MsfTrack` / `MsfCloneTrack` の encode / validate をその上に委譲する。`shiguredo-rust` 規約の「トレイトを作らない」に抵触しない範囲で共通化する。`src/msf.rs` の分割 (catalog / timeline) は別 issue とする。

## 完了条件

- `MsfTrack` / `MsfCloneTrack` で JSON メンバー列と検証ロジックが共有されていること
- 既存のエンコード / デコード結果が変わらないこと
- 既存テスト / PBT がすべて通ること

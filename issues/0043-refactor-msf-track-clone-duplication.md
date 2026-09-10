# MsfTrack / MsfCloneTrack のエンコード・検証の重複を解消する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-msf-track-clone-duplication
- Polished: 2026-09-10

## 目的

`src/msf.rs` の `MsfTrack` と `MsfCloneTrack` の encode（`DisplayJson`）と検証の重複を減らし、フィールド追加時の修正漏れを防ぐ。`src/msf.rs`（約 3,379 行）の保守負債を下げる。

## 現状

- `MsfTrack` は 41 フィールド、`MsfCloneTrack` は 42 フィールドを持つ。両者の JSON メンバー列は完全一致ではない。
  - `MsfCloneTrack` のみ `parentNamespace` を出力する。
  - `depends` / `accessibility` は `MsfTrack` が空なら省略し、`MsfCloneTrack` は `Some` なら空でも出力する（親からの継承と空上書きを区別するため）。
  - `packaging` / `is_live` は `MsfTrack` が必須、`MsfCloneTrack` が `Option`。
- `impl DisplayJson for MsfTrack` と `impl DisplayJson for MsfCloneTrack` が個別実装で、メンバー書き出しがほぼ重複している。
- decode は既存の内部型 `CommonTrackFields`（39 フィールド、`src/msf.rs` の `struct CommonTrackFields`）と `decode_common_track_fields` で一部共通化済み。この型は「継承可能フィールドを `Option` にした」ものに相当し、39 という数値はこの型のフィールド数である。
- 検証は `validate_full_track` と `validate_clone_track_fragment` が別実装で部分重複する。ただし両者は意図的に非対称である。
  - full は `parentName` を拒否し、encryption 欠如と timeline の `depends` / `mimeType` を要求し、`validate_media_track_fields` を呼ぶ。
  - clone は親から継承し得るため欠如を許可し、`trackDuration` を `is_live == Some(true)` で判定する。
  - `validate_media_track_fields` / `reject_coexisting_target_latency_buffers` / `validate_lang_tag` は既に共有されている。

対象シンボル: `MsfTrack` / `MsfCloneTrack` / `CommonTrackFields` / `DisplayJson for MsfTrack` / `DisplayJson for MsfCloneTrack` / `decode_track` / `decode_clone_track` / `decode_common_track_fields` / `validate_full_track` / `validate_clone_track_fragment` / `MsfCloneTrack::into_track`。

## 設計方針

- 既存の `CommonTrackFields` を拡張・再利用するか、encode / validate 用の内部フラグメント型を新設するかを決める。新設する場合は `CommonTrackFields` と統合し、フラグメント型が 2 つに増えないようにする。公開範囲は crate 内部に閉じる。
- 共通化するのは両型で同一の規則（メンバー書き出し、`validate_lang_tag`、`reject_coexisting_target_latency_buffers`、`eventType` 検証など）に限る。必須可否・継承可否の規則は従来どおり各 `decode_*` / `validate_*` に残す。
- 公開型 `MsfTrack` / `MsfCloneTrack` のフィールド構成は変えない（内部ヘルパへの委譲に留める）。フィールド構成を変える場合は破壊的変更として `CHANGES.md` に記載する。どちらを取るか実装前に決める。
- 既存の「encode の drift は roundtrip テストで検出する」方針では、フィールド追加時にサンプラへ自動で乗らないケースを取りこぼすため、共通化後に drift を検出するテストを追加する。
- `src/msf.rs` の分割（catalog / timeline）は別 issue とする。

## 依存

- open の `issues/0031-bug-msf-islive-roundtrip.md` は同じ `DisplayJson for MsfTrack` を変更する。0031 を先に取り込み、`isLive=false` で `targetLatency` / `buffers` を出さない挙動を維持したうえで共通化する。0031 の完了を前提とする。

## 完了条件

- 共通メンバー列の書き出しが単一の関数または単一の `DisplayJson` 実装に集約され、`MsfTrack` / `MsfCloneTrack` の両方がそれを呼んでいること
- 両型で同一の検証規則が単一の関数に集約され、`decode_track` / `decode_clone_track` / `validate_full_track` / `validate_clone_track_fragment` / `into_track` が必要に応じてそれを呼んでいること
- full / clone 固有の必須・継承規則と、`parentNamespace` と空配列の出力規則が維持されていること
- 0031 の `isLive=false` の encode 規則が維持されていること
- 共通化の drift を検出するテストを `tests/test_msf/` または `pbt/tests/prop_msf.rs` に追加すること
- 既存テスト / PBT がすべて通ること

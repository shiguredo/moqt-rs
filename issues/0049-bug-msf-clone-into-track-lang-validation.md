# MsfCloneTrack::into_track の解決結果で lang を検証する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-clone-into-track-lang-validation
- Polished: {YYYY-MM-DD}

## 目的

`MsfCloneTrack::into_track` の継承解決結果に `lang` (BCP 47) の検証を適用し、encode 時まで不正に気付けない非対称をなくす。

## 現状

`src/msf.rs` の `MsfCloneTrack::into_track` は、解決後の `MsfTrack` に対して `validate_media_track_fields` のみを呼び、
`validate_lang_tag` (draft-ietf-moq-msf-01 §5.2.32 (Language)) を呼ばない。そのため clone で不正な `lang` を設定すると
`apply_delta` (clone 経路) は成功し、その後の encode (`validate_full_catalog_for_encode` → `validate_full_track` → `validate_lang_tag`) で初めて `InvalidCatalog` になる。

`add` 経路は 0032 で `validate_full_track` を呼ぶようになったため、clone 経路だけが非対称として残っている。

根拠: draft-ietf-moq-msf-01 §5.2.32 (Language): BCP 47 言語タグ。

## 設計方針

`into_track` の末尾検証を `validate_media_track_fields(&track)?` から `validate_full_track(&track)?` に置き換える (解決済みで `parent_name` は `None` なので `validate_full_track` の parentName 検査を通る)。これにより add / clone / encode で同一の検証関数を使う。

## 完了条件

- clone の継承結果が不正な lang を持つ場合、`apply_delta` 時点で `InvalidCatalog` として拒否されること
- 既存の正常系が維持されること
- 回帰テストが `tests/test_msf/` に追加されていること

# MsfCloneTrack::into_track の解決結果で lang を検証する

- Created: 2026-09-10
- Completed: 2026-09-17
- Branch: feature/fix-msf-clone-into-track-lang-validation
- Polished: 2026-09-11

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
あわせて `into_track` の `# Errors` を `validate_full_track` の検証範囲に合わせて更新し、新たに検証対象となる §5.2.32 (Language) を追記する。

## 完了条件

- clone の継承結果が不正な lang を持つ場合、`apply_delta` 時点で `InvalidCatalog` として拒否されること
- `into_track` の `# Errors` が `validate_full_track` の検証範囲 (少なくとも §5.2.32 Language) を反映していること
- 既存の正常系が維持されること
- 回帰テストが `tests/test_msf/` に追加されていること

## 解決方法

`src/msf.rs` の `MsfCloneTrack::into_track` の末尾検証を `validate_media_track_fields` から
`validate_full_track` に置き換え、add 経路と同じ検証範囲に揃えた。

- 解決済みトラックは `parent_name` が `None` なので `validate_full_track` の parentName 検査は通る
- これにより clone で設定・継承した不正な `lang` (§5.2.32 BCP 47) が `apply_delta` 時点で
  `InvalidCatalog` になる
- `# Errors` に §5.2.32 (Language) を追記し、検証が `validate_full_track` に一本化されて
  いることを明記した

回帰テストを `tests/test_msf/delta_apply.rs` に 2 本追加した。

- `apply_delta_clone_invalid_lang_rejected`: clone 側で `lang = "1"` を指定した場合に拒否される
- `apply_delta_clone_inherited_invalid_lang_rejected`: 親の `lang = "-en"` を継承した場合に拒否される

どちらも修正前は失敗する (拒否されず成功する) ことを確認してから修正を入れた。
テストは `invalid language tag` を含む理由で拒否されることまで検証しており、
「親トラックが見つからない」エラーと区別している。

検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo fmt --all -- --check` の通過で確認した。

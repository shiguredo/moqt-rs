# 受信 subgroup Object フィルタのテストファイルを分割しヘルパを集約する

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-subgroup-filter-test-split
- Polished: {YYYY-MM-DD}

## 目的

受信 subgroup Object のフィルタ再適用テストが 1 ファイルに集中し、共有できるヘルパも既存テストと重複している。テストの保守性を戻し、次にフィルタ経路を変更したときの追従コストを下げる。テスト内容は変えない。

## 現状

- `tests/test_session/subgroup_object_filter.rs` が 2398 行で、`mod` 分割もセクション見出しもなくテストとヘルパが一直線に並んでいる（`shiguredo-rust` は「テストファイルが長くなった場合はファイル内で `mod` を使って分割する」としている）。
- `send_and_recv_subscribe` / `complete_subscribe` が `tests/test_session/shared_track_alias.rs` と重複している。
- `normal_object` / `status_object` が `tests/test_session/data_stream.rs` と重複している。
- `SubgroupHeader` のリテラルが多数のテストで毎回書かれており、差分が alias / group / subgroup / priority だけでも全フィールドを繰り返している。

## 設計方針

- 共有できるヘルパ（subscribe 確立、`DecodedSubgroupObject` 生成、`SubgroupHeader` 生成）を `tests/test_session.rs` の共通ヘルパへ集約し、各テストファイルの重複定義を削除する。
- テストを関心ごと（routing / cancellation・discard / group end / override ライフサイクル / priority）に `mod` で分割し、ファイル先頭に構成を書く。
- テスト名・アサーション・期待値は変えない。分割時に共有ヘルパへ寄せることでテストの意味が変わらないよう注意する。

## 完了条件

- 重複していたテストヘルパが集約され、重複定義が残っていないこと
- `subgroup_object_filter` のテストが分割後の構成で全て通り、検証内容が変わっていないこと
- `cargo test --workspace` が通ること

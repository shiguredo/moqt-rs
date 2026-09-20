# MSF の decode / encode エラーケーステストを分割する

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-split-msf-error-cases
- Polished: 2026-09-27

## 目的

`tests/test_msf/error_cases.rs` が肥大化し、複数のテーマが 1 ファイルに混在している。テーマ単位に分割して 1 ファイル 1 テーマにし、レビュー・変更時の影響範囲を読みやすくする。

## 現状

`tests/test_msf.rs` が `#[path = "test_msf/error_cases.rs"] mod error_cases;` で読み込んでいる。`error_cases.rs` は先頭の `use super::*;` で共通の import を共有し、次のテーマを 1 ファイルに持つ (先頭の見出し無しブロックも含む)。

- カタログ基本構造と timeline の decode 検証 (先頭の見出し無しブロック。`missing_version` から `depends_entry_not_string` までの version / tracks / name / packaging / isLive の必須検証、JSON 構造、media / event timeline、depends)
- lang BCP 47 軽量バリデーション
- targetLatency / buffers の相互制約
- initDataList
- delta update 新構造
- codec / role に応じた必須フィールド
- framerate の非有限値拒否
- authInfo value_raw の encode 時検証
- encode 時の完全カタログ検証
- encode 時のデルタ更新検証

先頭の見出し無しブロックのうち `render_group_different_target_latency_rejected` から `render_group_omitted_between_different_target_latency_rejected` までの 7 テストは group の targetLatency の一致と省略を検証しており、テーマとしては targetLatency / buffers の相互制約に属するため、分割時には同テーマのファイルへ移す。

また、delta update 新構造の節の末尾には delta update ではない decode 検証が混在している。
`init_data_list_missing_id_rejected` / `init_data_list_missing_type_rejected` /
`init_data_list_missing_data_rejected` は initDataList テーマへ、
`buffers_non_number_value_rejected` / `alt_group_different_buffers_rejected` は
targetLatency / buffers の相互制約テーマへ、`event_timeline_missing_event_type_rejected` /
`media_timeline_missing_depends_rejected` / `media_timeline_wrong_mime_type_rejected` /
`event_type_with_non_eventtimeline_rejected` / `eventtimeline_missing_depends_rejected` /
`eventtimeline_wrong_mime_type_rejected` はカタログ基本構造と timeline の decode 検証テーマへ
移動し、残った clone / add 系のテストだけが delta update 新構造テーマに残る。

codec / role に応じた必須フィールド節の末尾にある `parent_name_in_track_rejected` /
`parent_namespace_in_track_rejected` は必須フィールドではなく通常トラックの禁止フィールド
制約であり、カタログ基本構造と timeline の decode 検証テーマへ移す。

ファイルは 2208 行あり、複数の節見出しコメントで区切られている。`tests/test_msf/` 配下の他ファイル (`catalog_decode.rs` / `track_fields.rs` / `media_timeline.rs` 等) は 1 テーマ 1 ファイルで分割済みである (`event_timeline.rs` には nojson の深さ制限 (`MAX_NESTING_DEPTH`) の検証も含まれるが、本 issue のテーマとは独立した関心事である)。`error_cases.rs` だけが複数のテーマを 1 ファイルに持っている。

なお、open の `issues/0146` は `tests/test_msf/error_cases.rs` へのテスト追加を完了条件にしている。本 issue を先行させると 0146 の追加先が分割後の codec / role ファイルへ変わるため、実施順に依存があり、`issues/0146` を未着手のまま本 issue を実施する場合は参照先の読み替えが必要である。

## 設計方針

既存の分割方針に合わせ、テーマ単位で `tests/test_msf/` 配下の新しいファイルへ移動する。`tests/test_msf.rs` に対応する `#[path = ...] mod ...;` を追加し、`error_cases.rs` とその mod 宣言は削除する。

- 移動先のファイル名はテーマが分かる英語スネークケース (snake_case) にする (既存の `catalog_decode.rs` / `event_timeline.rs` と同様)
- 各ファイルは先頭で `use super::*;` を宣言し、共通 import は親モジュール (`tests/test_msf.rs`) から引き続き共有する
- テスト名・テスト内容・テスト数は変更しない (純粋な移動とする)
- 複数ファイルから使うヘルパーが生じた場合は親モジュールに置く

## 完了条件

- 分割後の各ファイルが 1 テーマに絞られていること
- `tests/test_msf.rs` の mod 宣言が分割後のファイル構成と一致していること
- テストの総数と内容が分割前後で変わらないこと
- `make test` / `make clippy` / `make fmt` が通ること

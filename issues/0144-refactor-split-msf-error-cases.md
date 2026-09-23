# MSF の decode / encode エラーケーステストを分割する

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-split-msf-error-cases
- Polished: {YYYY-MM-DD}

## 目的

`tests/test_msf/error_cases.rs` が肥大化し、複数のテーマが 1 ファイルに混在している。テーマ単位に分割して 1 ファイル 1 テーマにし、レビュー・変更時の影響範囲を読みやすくする。

## 現状

`tests/test_msf.rs` が `#[path = "test_msf/error_cases.rs"] mod error_cases;` で読み込んでいる。`error_cases.rs` は先頭の `use super::*;` で共通の import を共有し、次のテーマを 1 ファイルに持つ。

- lang BCP 47 軽量バリデーション
- targetLatency / buffers の相互制約
- initDataList
- delta update 新構造
- role に応じた必須フィールド
- framerate の非有限値拒否
- authInfo value_raw の encode 時検証
- encode 時の完全カタログ検証
- encode 時のデルタ更新検証

ファイルは 1779 行あり、複数の節見出しコメントで区切られている。`tests/test_msf/` 配下の他ファイル (`catalog_decode.rs` / `track_fields.rs` / `media_timeline.rs` 等) は 1 テーマ 1 ファイルで分割済みであり、`error_cases.rs` だけがこの方針から外れている。

## 設計方針

既存の分割方針に合わせ、テーマ単位で `tests/test_msf/` 配下の新しいファイルへ移動する。`tests/test_msf.rs` に対応する `#[path = ...] mod ...;` を追加し、`error_cases.rs` とその mod 宣言は削除する。

- 移動先のファイル名はテーマが分かる英語ケバブケースにする
- 各ファイルは先頭で `use super::*;` を宣言し、共通 import は親モジュール (`tests/test_msf.rs`) から引き続き共有する
- テスト名・テスト内容・テスト数は変更しない (純粋な移動とする)
- 複数ファイルから使うヘルパーが生じた場合は親モジュールに置く

## 完了条件

- 分割後の各ファイルが 1 テーマに絞られていること
- `tests/test_msf.rs` の mod 宣言が分割後のファイル構成と一致していること
- テストの総数と内容が分割前後で変わらないこと
- `make test` / `make clippy` / `make fmt` が通ること

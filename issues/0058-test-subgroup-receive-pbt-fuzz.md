# 受信 subgroup 経路の PBT と fuzz を追加する

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/test-subgroup-receive-pbt-fuzz
- Polished: {YYYY-MM-DD}

## 目的

受信 subgroup 経路に「候補ごとのフィルタ再適用と帰属」「stream 会計（open 数）」「delivery timeout override のライフサイクル」が加わったが、固定ケースの単体テストでしか検証されていない。ランダムな操作列で不変条件を検証し、共有 Track Alias まわりの不整合の再発を防ぐ。

## 現状

- `pbt/tests/prop_session/` には受信 subgroup の帰属・会計・override を対象にした PBT がない（`datagram.rs` に datagram 経路の PBT はある）。
- `fuzz/fuzz_targets/fuzz_session.rs` の `Op` は data stream type の通知と close のみで、subgroup header / object の受信操作がない。
- 単体テストでは、帰属先 subscription の forget、キャンセル、Malformed 検出、override 削除の組合せが個別ケースでしか固定されていない。

## 設計方針

- PBT（noprop）で次の不変条件を検証する。
  - `open_incoming_subgroup_count` が incoming の生きた data stream 数 (subgroup + fill fetch) と一致し、全 stream 終端 / forget 後に 0 になる
  - 全 stream 終端 / forget 後に delivery timeout override が残らない
  - 帰属先が「候補順で最初の非キャンセルかつ `object_passes_filters` 合格」で決定的である
  - `FilteredOut` / `Discarded` の Object が subscription スコープの状態（`largest_received_location` / `ended_groups` / Object tracker）を更新しない
- fuzz の `Op` に subgroup header / object / close の受信操作を追加し、panic と状態破壊を検出する。decode 済みの構造体を直接渡す操作列でよい。
- 単体テストと同じ固定ケースを PBT で重複させない。PBT はサンプラーで入力の組合せを生成し、単体テストはエラーパス・境界値に残す。
- 既存テストの流儀に合わせ、モックやスタブは使わない。

## 完了条件

- 上記の不変条件が `pbt/tests/prop_session/` で検証されていること
- `fuzz_session` が subgroup 経路の操作列を生成し、クラッシュしないこと
- `cargo test -p pbt` と `cargo check --manifest-path fuzz/Cargo.toml` が通ること

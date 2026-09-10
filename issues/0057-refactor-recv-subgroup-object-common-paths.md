# recv_subgroup_object を分割し候補評価とキャンセル後始末を共通化する

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-subgroup-object-common-paths
- Polished: {YYYY-MM-DD}

## 目的

受信 subgroup 経路に共有 Track Alias の再帰属・キャンセル処理が加わり、同型ロジックの重複が増えた。重複は修正漏れを繰り返し生んでおり、次に触る前に構造を整理する。挙動は変えない。

## 現状

- `Session::recv_subgroup_object` が stream 状態の取得、FirstObjectId の subgroup_id 解決、`record_priority`、候補評価、Malformed Track の終端、delivery timeout override の登録、subscription スコープの状態更新、tracker 検証、帰属先の記録を 1 関数で直列に処理しており 300 行を超える。
- 候補評価ループ（候補順に評価し、キャンセル由来 `Terminated` を除外して最初に通過した subscription を採用する）が `recv_subgroup_header` / `recv_subgroup_object` / `recv_object_datagram` の 3 箇所に同型で存在する。
- キャンセル済み stream の後始末（`note_incoming_subgroup_stream_closed` + `remove_subgroup_delivery_timeout_override` + SubgroupTracker の終端記録 + `retain_discarded_stream_id`）が `report_mid_object_fin` / `recv_data_stream_closed` / `send_data_stream_stop_sending` / `register_discarded_stream` の 4 箇所に同型で存在する。
- `IncomingDataStream::Subgroup` の明示フィールド列挙があちこちに散在し、フィールド追加のたびに複数箇所を直す必要がある。

## 設計方針

- 候補評価を 1 つの private ヘルパへ集約する。header / object / datagram の差は `ObjectFilterInput` の組み立てだけに閉じ込める。
- キャンセル後始末を 1 つの private ヘルパへ集約し、破棄分岐へ入る条件の判定も 1 箇所にまとめる。
- `recv_subgroup_object` を「stream 状態の取得」「subgroup_id の解決」「帰属判定」「受理後の状態更新」の段階に分割する。
- 挙動・公開 API・テストの期待値は変えない。整理の途中で仕様判断が必要になった場合は、勝手に決めず停止して確認する。

## 完了条件

- 候補評価ループとキャンセル後始末の同型ロジックがそれぞれ 1 箇所に集約されていること
- `recv_subgroup_object` が段階ごとの private 関数に分割されていること
- `cargo test --workspace` と PBT が通り、テストの期待値が変わっていないこと
- 公開 API と `CHANGES.md` に変更がないこと（機能変更を伴わない整理のため `### misc` 扱いとする）

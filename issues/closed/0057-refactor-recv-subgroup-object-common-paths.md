# recv_subgroup_object を分割し候補評価とキャンセル後始末を共通化する

- Created: 2026-09-11
- Completed: 2026-09-18
- Branch: feature/refactor-recv-subgroup-object-common-paths
- Polished: 2026-09-18

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

## 解決方法

`src/session/data.rs` の受信 subgroup 経路を、挙動を変えずに整理した。候補評価ループを
`Session::evaluate_candidates` の 1 箇所へ、キャンセル済み stream の後始末を
`Session::discard_cancelled_subgroup_stream` と `Session::cleanup_discarded_subgroup_stream`
の 1 箇所へ集約し、`Session::recv_subgroup_object` を段階ごとの private 関数へ分割した。
公開 API・wire 上の挙動・テストの期待値は変えていない。

- `Session::evaluate_candidates`: `recv_subgroup_header` / `recv_subgroup_object` /
  `recv_object_datagram` に同型で存在した候補評価ループ（候補順にフィルタを評価し、
  キャンセル由来 `Terminated` を除外して最初に通過した subscription を採用し、合格が
  キャンセル由来のみなら破棄として扱う）を集約した。3 経路の差は、呼び出し側が組み立てて
  渡す `CandidateFilterInput` だけに閉じ込めた。header は `header_passes_filters` による
  Group 単位の評価、object / datagram は `ObjectFilterInput` による `object_passes_filters`
  の評価になる。候補ごとに解決する Publisher Priority も入力の組み立てに含めた
- `Session::discard_cancelled_subgroup_stream`: 破棄分岐へ入る条件（キャンセル由来
  `Terminated` に属し、現在も生きた Object の帰属先が無い）の判定と後始末を集約した。
  `report_mid_object_fin` / `recv_data_stream_closed` / `send_data_stream_stop_sending` の
  3 箇所がこれを使う。後始末の本体は `Session::cleanup_discarded_subgroup_stream`
  （受信 stream 会計の返却と per-subgroup delivery timeout override の削除）で、
  破棄条件を判定せずに既存 `Subgroup` variant を後始末する `register_discarded_stream` も
  同じ本体を共用する。wire 構造の終端記録（FIN / RESET / STOP_SENDING の別）は終端の種類で
  異なるため、呼び出し側に残した
- `Session::recv_subgroup_object`: 「stream 状態の取得」(`take_incoming_subgroup_object`)、
  「subgroup_id の解決」(`resolve_object_subgroup_id`)、「帰属判定」
  (`attribute_subgroup_object`)、「受理後の状態更新」(`accept_subgroup_object`) の 4 段階に
  分割した。各段階は「どの状態をどこまで更新するか」を doc に持ち、Malformed Track
  条件 1 / 2 の検出順と優先順位、キャンセル由来候補の扱いは分割前と同じに保った
- `IncomingDataStream::Subgroup { ... }` のフィールド列挙を `IncomingSubgroupStream` 型と
  `IncomingDataStream::as_subgroup` / `subgroup_mut` に集約した。フィールドを追加するときに
  直す箇所が型定義とアクセサだけで済む

`cargo test --workspace` の passed は 1590 件（42 テストバイナリ）で前後で変わらず、
テスト名の一覧も一致している（PBT は `cargo test -p pbt` で 80 件）。検証は
`cargo test --workspace`、`cargo test -p pbt`、
`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check`、
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps` で行い、すべて通った。

統合にあたり判断した点を残す。header は Object ID と Object Properties が受信時点で未確定で
あり `ObjectFilterInput` では評価できない（`header_passes_filters` を使う）ため、3 経路の
入力の差は `CandidateFilterInput` の enum で表現した。Object 経路の variant は
`ObjectFilterInput` そのものであり、差は入力の組み立てだけに閉じている。
`register_discarded_stream` は破棄条件を判定せずに既存 `Subgroup` variant を後始末するため、
条件判定込みの `discard_cancelled_subgroup_stream` ではなく後始末だけの
`cleanup_discarded_subgroup_stream` を呼ぶ形にした（条件判定を足すと、現状到達しない経路の
挙動を変えることになるため）。

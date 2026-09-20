# 受信 Subgroup Object に Object 単位フィルタを再適用する

- Created: 2026-09-10
- Completed: 2026-09-11
- Branch: feature/change-subgroup-object-filter-reapplied
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §3.1 (Subscriptions) の「subscriber は受信 Object がどの subscription のものか判別するためフィルタを再適用する」規則と §3.3.3 (Combining Filters) の Pass 評価を、受信 subgroup 経路でも満たす。フィルタを通過しない Object が Application へ渡るのを防ぎ、共有 Track Alias の候補を Object ごとに正しい subscription へ帰属させる。

## 現状

`src/session/data.rs` の `recv_subgroup_object` は、Object 単位のフィルタを評価していない。受信 subgroup 経路ではヘッダ受信時の `header_passes_filters` (`src/session/data.rs` → `src/session/subscription/validation.rs`) だけで候補を絞る。この関数は Forward / SUBGROUP_FILTER / PRIORITY_FILTER と Location Filter の Group
部分しか評価せず、OBJECTID_FILTER / OBJECT_PROPERTY_FILTER / Location Filter の Object 部分は評価対象外である。加えて `SubgroupIdMode::FirstObjectId` は subgroup_id が未解決のため、`header_passes_filters` は SUBGROUP_FILTER を素通りさせる (subgroup_id が `None` なら無条件で通す)。

対して datagram 経路の `recv_object_datagram` は、受信 Object ごとに `resolve_peer_track_alias` の候補すべてへ `object_passes_filters` を適用し、最初に通過した subscription へ帰属させて `TrackDataAcceptance` を返す。送信側の `send_subgroup_object` も `object_passes_filters` を呼ぶ。受信 subgroup 経路だけが抜けている。

根拠 (draft-ietf-moq-transport-21 §3.1):

> "Because subscriptions can share a Track Alias, the subscriber re-applies each subscription's filter to determine which subscription a received Object belongs to."

実害は 2 つある。1 つ目は、同一 Track Alias に Object 単位フィルタだけが異なる複数 subscription を張った場合、header 時点では判別できないため、header が固定した subscription のフィルタだけを再適用すると他の subscription に属する Object を破棄してしまうこと (datagram 経路は Object ごとに振り分けている)。2 つ目は、非同期なフィルタ更新で購読範囲外になった Object を Application へ渡してしまうこと。

受信 subgroup 経路の Object 単位フィルタを検証するテストは存在しない。送信側 subgroup の Object 単位フィルタテストは `tests/test_session/object_filter_pass.rs` に、header 時点の SUBGROUP_FILTER 振り分けは `tests/test_session/shared_track_alias.rs` にある。

## 設計方針

- `recv_subgroup_object` は header 時に `IncomingDataStream::Subgroup` へ記録した request_id だけを対象にせず、`resolve_peer_track_alias` で得た候補すべてに `object_passes_filters` を再適用し、最初に通過した Established な subscription へ Object を帰属させる。候補ループとキャンセル由来候補の扱いは datagram の `recv_object_datagram`
  と同じにし、通過がキャンセル由来候補のみなら `Discarded`、どの候補も通過しなければ `FilteredOut` を返す。
- header 時に記録した request_id は stream 会計 (`note_incoming_subgroup_stream_opened` / `note_incoming_subgroup_stream_closed`、`recv_data_stream_closed` の FIN / RESET 処理) に維持し、Object の帰属判定で上書きしない。
- `SubgroupIdMode::FirstObjectId` は `subgroup_id` の解決後にフィルタを評価し、SUBGROUP_FILTER も解決済み subgroup_id で評価する。
- フィルタ不通過でも wire 構造の整合と生存監視に必要な更新は行う: `first_object_received` / `last_object_id` (FIN 時の Group 終端確定)、FirstObjectId の `subgroup_id` 解決と `peer_subgroups.open` / `record_priority`、`check_object_after_fin` (Malformed Track)、`data_stream_last_activity_ms` (DATA_STREAM_TIMEOUT の誤発火防止)。
- subscription スコープの更新 (`record_largest_received_location`、`record_object_status_end`、`peer_object_properties` / `peer_object_fields`、先頭 Object の delivery timeout override) は帰属した subscription にのみ適用し、不通過時は更新しない。
- 戻り値を `Result<TrackDataAcceptance, SessionError>` に変更する (`Accepted` / `FilteredOut` / `Discarded`。header 受理済み stream では `UnknownTrackAlias` を返さない)。公開 API の後方互換のない変更のため、ブランチは `feature/change-...` とし、`CHANGES.md` の `## develop` に `[CHANGE]` として記載する (0025 と同じ扱い)。
- 影響範囲と呼び出し側の追従: `examples/moqt-transport/src/moqt_client.rs` のラッパー、`examples/moqt-subscriber/src/pipeline.rs` の受信ループ、`tests/test_session/` の既存呼び出し、`README.md`、`skills/shiguredo-moqt/SKILL.md`。example は `FilteredOut` の Object でも payload を読み出して消費し、Application へ渡さず stream を継続する
  (payload を残すと次の `SubgroupStreamDecoder::try_decode_object` が `ProtocolViolation` になる)。

## 完了条件

- 受信 subgroup 経路で OBJECTID_FILTER / OBJECT_PROPERTY_FILTER / Location Filter の Object 部分と、FirstObjectId の SUBGROUP_FILTER が評価されること
- 同一 Track Alias の候補が Object ごとに振り分けられ、他の subscription に属する Object が破棄されないこと
- フィルタ不通過 Object が subscription 状態を更新しないこと
- 不通過の破棄が `TrackDataAcceptance` で呼び出し側へ伝わること
- 受信 subgroup 経路の Object 単位フィルタのテストが `tests/` に追加されていること
- `examples/` が `FilteredOut` の payload を消費して継続し、`cargo test --workspace` と PBT が通ること
- `CHANGES.md` の `## develop` に `[CHANGE]` として記載されていること

## 解決方法

受信 subgroup 経路で Object 単位のフィルタ再適用と帰属判定を行うようにした。

- `src/session/data.rs` の `recv_subgroup_object` の戻り値を `TrackDataAcceptance` に変更し、`resolve_peer_track_alias` の候補すべてへ `object_passes_filters` を再適用して、最初に通過した（キャンセル由来 `Terminated` でない）subscription へ Object を帰属させるようにした。通過がキャンセル由来候補のみなら `Discarded`、どの候補も通らなければ `FilteredOut` を返す。
- `SubgroupIdMode::FirstObjectId` は `subgroup_id` を解決してからフィルタを評価し、`SUBGROUP_FILTER` も解決済み ID で評価するようにした。
- フィルタ不通過でも wire 構造の整合と生存監視に必要な更新（`first_object_received` / `last_object_id` / subgroup_id 解決 / `peer_subgroups` の記録 / Malformed Track 検出 / data stream activity）は継続し、subscription スコープの更新（`largest_received_location` / Object Status 終端 / Object tracker / 先頭 Object の delivery timeout override / `ended_groups`）は帰属先にのみ適用する。
- 共有 Track Alias の周辺挙動を整備した。
  - `PRIORITY_FILTER` は stream が保持する header priority を候補ごとに解決して評価する。
  - Malformed Track 条件 2 の終端対象は帰属先にする。
  - delivery timeout override は帰属先に登録し全終端経路で削除する。
  - キャンセル済み所有者の stream でも生きた帰属先があれば再帰属を継続し、`report_mid_object_fin` / FIN / RESET / STOP_SENDING の分岐と stream 会計を整合させる。
  - 帰属先が回収済みの場合の生存判定と、`forget_subscription` 時の生きた他候補への移管を行う。
- `examples/moqt-transport` / `examples/moqt-subscriber` を戻り値と payload 消費の契約に追従し、`README.md` / `skills/shiguredo-moqt/SKILL.md` を更新した。
- `CHANGES.md` の `## develop` に `[CHANGE]` を追記した。
- テストは `tests/test_session/subgroup_object_filter.rs`、`tests/test_session/subscription/terminated_discard.rs`、`src/session/tests.rs` に追加・更新した。

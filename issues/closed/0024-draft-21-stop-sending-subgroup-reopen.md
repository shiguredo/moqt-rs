# STOP_SENDING 後の Subgroup 再オープンを Forward 0→1 更新時のみ許可する

- Created: 2026-09-10
- Completed: 2026-09-13
- Branch: feature/fix-stop-sending-subgroup-reopen
- Polished: 2026-09-10

## 目的

draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) の SHOULD NOT に従う。STOP_SENDING を受けた Subgroup への再オープンを、その後に Forward State が 0 から 1 へ変わった場合に限定し、それ以外の再オープンを抑止する。

## 現状

- `src/subgroup_tracker.rs` の `can_reopen` は `StoppedByPeer` と `Reset` の両方を無条件に再オープン可能として扱う。`send_subgroup_header` / `send_subgroup_object` の `my_subgroups.open` 呼び出し元にも Forward 0→1 の確認がない。
- STOP_SENDING の記録は終端状態と同居している。`recv_data_stream_stop_sending` が `mark_stop_sending` で `StoppedByPeer` を記録した後、§11.3.2 の "The publisher SHOULD respond with a reset." に従って `reset_outgoing_data_stream` を呼ぶと、`send_data_stream_closed(Reset)` → `mark_reset` が `StoppedByPeer` を `Reset`
  に上書きする。`can_reopen(Reset)` は true のため、STOP_SENDING → reset の通常経路では停止の事実が消える。
- Forward State は現在値のみを保持する。停止時に Forward State が 1 だった場合、その後の 0→1 遷移がなくても「現在値 1」の静的な判定では再オープンを許可してしまう。§11.3.2 が求めるのは「STOP_SENDING の後に 0→1 へ変える REQUEST_UPDATE を受けた場合」という順序付きイベントである。
- `SubgroupTracker` のキーは `(track_alias, group_id, subgroup_id)` で request_id を持たない。alias は複数 subscription で共有されうるため、Forward の 0→1 を alias だけで解除対象に紐付けると、別 subscription の停止を誤って解錠しうる。`recv_data_stream_stop_sending` は `OutgoingDataStream.request_id` を取得できる。
- FirstObjectId モードでは subgroup_id が先頭 Object 受信時に解決されるため、gating は `send_subgroup_header` だけでなく `send_subgroup_object` の解決経路にも必要である。
- `src/subgroup_tracker.rs` のコメントは「仕様は reset の理由で区別していない」としているが、STOP_SENDING については §11.3.2 が 0→1 条件を明示しており正確ではない (DELIVERY_TIMEOUT 起因の reset は §5.2 が別途 SHOULD NOT を定める)。

根拠 (draft-ietf-moq-transport-21 §11.3.2):

> "A publisher that receives a STOP_SENDING on a Subgroup stream SHOULD NOT attempt to open a new stream to deliver additional Objects in that Subgroup. However, if the publisher subsequently receives a REQUEST_UPDATE that changes the Forward State from 0 to 1, it MAY open a new stream..."

## 設計方針

- 対象は自側が publisher として送る Subgroup (`my_subgroups` と送信 API) の再オープンとする。受信側 (`peer_subgroups`) の受理挙動は変更しない (peer の SHOULD NOT 違反をセッションエラーにしない)。DELIVERY_TIMEOUT 起因の reset (§5.2 の SHOULD NOT) も本 issue の対象外とする。
- STOP_SENDING を受けた Subgroup を、終端状態 (`StoppedByPeer` / `Reset`) とは独立した「再オープン禁止」状態として Session が request_id 単位で保持する。`SubgroupTracker` の既存 public API (`can_reopen` / variant) は変更しない。
  - 記録: `recv_data_stream_stop_sending` が `OutgoingDataStream.request_id` と `(track_alias, group_id, subgroup_id)` を組にして記録する
  - 保持は終端状態の上書きに影響されない (STOP_SENDING → reset 後も禁止状態が残る)
- 解除は、当該 request の Forward State が 0 から 1 へ変わった REQUEST_UPDATE が受理されて REQUEST_OK を返した時点 (`src/session/subscription/dispatch.rs` の `send_ok_for_subscription` が pending の FORWARD を適用する箇所) で、その request の禁止エントリをすべて解除する。現在値が 1 であることでは解除しない。`send_publish_state_notify` など REQUEST_UPDATE
  以外の Forward 変更経路では解除しない。
- gating は送信 API の 2 経路に置く。禁止中は `my_subgroups.open` を呼ばずに `Err` (`SESSION_PROTOCOL_VIOLATION`、既存 `open` のエラーと同型) を返し、セッションは閉じない。
  - `send_subgroup_header` (Explicit / Zero モード)
  - `send_subgroup_object` の FirstObjectId 解決 (先頭 Object の subgroup_id 確定時)
- STOP_SENDING 後に FIN で終端した場合は `ClosedFin` のままで再オープン不可 (現行どおり)。
- subscription の `forget_subscription` 時に禁止エントリを破棄する。
- `src/subgroup_tracker.rs` の `can_reopen` doc コメントの「仕様は reset の理由で区別していない」を、§11.3.2 の STOP_SENDING 条件と §5.2 の DELIVERY_TIMEOUT 条件を区別した記述に修正する。引用の節番号・Appendix 番号が誤っている箇所も併せて確認する。

## 完了条件

- STOP_SENDING を受けた Subgroup について、Forward 0→1 の REQUEST_UPDATE 受理後でなければ再オープンが `Err` になること
- 停止時に Forward State が 1 だった場合は、その後の 0→1 遷移がない限り再オープンできないこと
- STOP_SENDING → reset → Forward 0→1 後は再オープンできること (reset が `StoppedByPeer` を上書きしても禁止状態が残ること)
- FirstObjectId モードでも同じ gating が働くこと
- alias を共有する別 subscription の Forward 0→1 では解除されないこと
- `tests/test_session/data_stream.rs` の `stop_sending_on_subgroup_stream_allows_reopen` と `condition1_priority_mismatch_terminates_subscription` (再オープン前提の箇所) が新挙動に追従し、`tests/test_session/subscription/request_update.rs` の `forward_0_to_1_allows_reopen_of_stopped_by_peer_subgroup` が維持されること
- 回帰テストが `tests/test_session/data_stream.rs` に追加され、`cargo test --workspace` と PBT が通ること
- `CHANGES.md` の `## develop` に `[FIX]` として記載されていること

## 解決方法

STOP_SENDING を受けた outgoing Subgroup を Session が request_id 単位で「再オープン禁止」として保持し、Forward State 0→1 の REQUEST_UPDATE が受理された時点でのみ解除するようにした。

- `src/session/core.rs`: `Session` に `stopped_outgoing_subgroups: HashMap<u64, HashSet<(u64, u64, Option<u64>)>>` を追加した。`Option<u64>` は FirstObjectId モードの未解決 subgroup_id (`None`) を表す。SUBSCRIBE 起点では動作し、PUBLISH 起点では peer subscriber の REQUEST_UPDATE に応答する経路がない既知の制限を doc に明記した。
- `src/session/data.rs`: `recv_data_stream_stop_sending` で `OutgoingDataStream.request_id` と
  `(track_alias, group_id, subgroup_id)` を記録する。`send_subgroup_header` と
  `send_subgroup_object` の FirstObjectId 解決経路で `outgoing_subgroup_reopen_blocked`
  (全 request の停止エントリを照合) により再オープンを拒否し、`SESSION_PROTOCOL_VIOLATION` を
  返してセッションは閉じない。
- `src/session/subscription/dispatch.rs`: `send_ok_for_subscription` が pending の FORWARD を適用する際、`forward_state == 0` から `1` への遷移のときのみ当該 request の停止エントリを解除する。
- `src/session/subscription.rs`: `forget_subscription` で停止エントリを破棄する。
- `src/subgroup_tracker.rs`: `can_reopen` の doc を「終端種別のみを見て Forward 条件は Session が判定する」旨に更新し、§5.2 の DELIVERY_TIMEOUT 条件との区別を明記した。Appendix の誤引用 (A.3 → A.4) も修正した。
- `tests/test_session/data_stream.rs` などに、Forward 0→1 前の拒否 / 0→1 後の許可、Forward 1 固定時の拒否、STOP → reset 後の拒否と 0→1 後の許可、FirstObjectId の拒否と解除、共有 alias の分離、キー次元 (alias / group)、forget 後の破棄、既存テストの追従を追加した。
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した。

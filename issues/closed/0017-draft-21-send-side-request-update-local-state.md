# 送信側 namespace / track subscription の REQUEST_UPDATE prefix を反映する

- Created: 2026-09-10
- Completed: 2026-09-12
- Branch: feature/fix-send-side-request-update-local-state
- Polished: 2026-09-10

## 目的

REQUEST_UPDATE で TRACK_NAMESPACE_PREFIX を変更したとき、送信側のローカル prefix を peer の REQUEST_OK に合わせて更新し、以後に届く NAMESPACE / NAMESPACE_DONE の suffix 解決と PUBLISH の購読紐付けを正しく行う。

## 現状

- `src/session/namespace/subscribe_namespace.rs` の `send_update_for_namespace_subscription` と `src/session/namespace/track_subscription.rs` の `send_update_for_track_subscription` は role / state の検証のみで、ローカル状態を更新しない。パラメータの scope 検証は呼び出し元の `send_request_update` (`src/session/subscription/send.rs`) が行う。
- 受信側は `handle_update_for_namespace_subscription` が `TRACK_NAMESPACE_PREFIX` のみを、`handle_update_for_track_subscription` が `FORWARD` / `TRACK_PROPERTY_FILTER` / `TRACK_NAMESPACE_PREFIX` をローカルへ反映する。送信側には prefix の反映がない。
- 送信側で prefix を更新すると `NamespaceSubscription::prefix` / `TrackSubscription::prefix` が古いまま残る。`src/session/core.rs` の `recv_request` は受信 PUBLISH を `is_prefix_of(&ts.prefix, &track_namespace)` で購読へ紐付けるため、prefix 更新後に届いた PUBLISH が紐付かず `active_track_aliases`
  に入らない。`handle_peer_namespace` / `handle_peer_namespace_done` は suffix のみを Application へ渡すため、Application の suffix 解決も古い prefix のままになる。`namespace_subscription()` / `track_subscription()` は不変参照のみで Application からも補正できない。
- 受信側は REQUEST_UPDATE の受信時点で反映してよい (responder が以後送る側)。一方 initiator 側で送信時点に適用すると、REQUEST_OK 受信前に届く旧 prefix 相対の NAMESPACE / NAMESPACE_DONE を新 prefix で誤解決する。

根拠 (draft-ietf-moq-transport-21 §9.5.2):

> "If the update is accepted, NAMESPACE and NAMESPACE_DONE messages following the REQUEST_OK will contain Track Namespace suffixes relative to the updated prefix."

## 設計方針

- 対象は `TRACK_NAMESPACE_PREFIX` による prefix 更新のみとする。
- 送信時はローカル prefix を変えず、要求した prefix を「確定待ち」として送信順に保持する。現行の `send_update_for_namespace_subscription` / `send_update_for_track_subscription` は `request_id` しか受け取らないため、`send_request_update` から検証済み `parameters` を渡すようシグネチャを変更する。
- 複数 outstanding の REQUEST_OK は同一 bidi request stream 上で送信順に届く。`handle_ok_for_namespace_subscription` / `handle_ok_for_track_subscription` の Established 分岐 (REQUEST_UPDATE_OK) で、確定待ちの先頭から適用する。
- REQUEST_OK 受信までは旧 prefix で NAMESPACE / NAMESPACE_DONE の suffix を解決する (確定待ちを適用しない)。
- SUBSCRIBE_TRACKS の PUBLISH は別 bidi stream で REQUEST_OK との順序保証がない。publisher が update を処理した後に新 prefix で送る PUBLISH を取りこぼさないよう、確定待ちの間は旧 prefix と確定待ち prefix の両方にマッチする PUBLISH を当該 TrackSubscription の `active_track_aliases` に登録する。
- REQUEST_ERROR 受信 / bidi 終端 / Terminated 遷移時は確定待ちを破棄する。
- prefix overlap は既存の `send_subscribe_namespace` / `send_subscribe_tracks` の作成時検査と同じ条件で送信前にローカル検査する。overlap する場合は `SESSION_PROTOCOL_VIOLATION` を返し、確定待ちも登録しない。
- 対象外:
  - `TrackSubscription::forward_state`: 受信側で書き込まれるが `src/` 内に読み手がなく、open issue 0042 が利用または削除を判断するため本 issue では扱わない
  - Range Filter (`0x25`-`0x28`): `TrackSubscription` に保存先がなく、受信側も `TRACK_PROPERTY_FILTER` (`0x29`) のみ保持するため本 issue では扱わない
  - `send_update_for_subscription` (SUBSCRIBE) の楽観適用: 既存挙動を変更しない

## 完了条件

- namespace / track subscription の両方で、prefix 更新の送信直後は旧 prefix のままで、REQUEST_OK 受信後に新 prefix になること
- REQUEST_OK 受信前に届いた NAMESPACE / NAMESPACE_DONE の suffix が旧 prefix で解決され、REQUEST_OK 受信後は新 prefix で解決されること
- SUBSCRIBE_TRACKS で確定待ちの間に新 prefix の PUBLISH が届いても `active_track_aliases` に登録されること
- REQUEST_ERROR / bidi 終端で確定待ちが破棄され、prefix が旧のままであること
- 送信側 prefix 更新の回帰テストが `tests/test_session/namespace/` に追加され、`cargo test --workspace` が通ること

## 解決方法

送信側 REQUEST_UPDATE の TRACK_NAMESPACE_PREFIX を、送信時にはローカルへ反映せず「確定待ち」キューへ積み、REQUEST_OK 受信時に適用するようにした。

- `src/session/core.rs`: Session に `pending_prefix_updates: HashMap<u64, VecDeque<Option<TrackNamespace>>>` を追加した。`None` は prefix 変更なしの更新で、REQUEST_OK と確定待ちの対応を送信順に保つ。SUBSCRIBE_TRACKS の PUBLISH 紐付けは旧 prefix と確定待ち prefix の両方でマッチさせる。
- `src/session/namespace.rs`: 確定待ちを踏まえた実効 prefix を返す `effective_prefix` と push / pop のヘルパを追加した。
- `src/session/subscription/send.rs`: `send_update_for_namespace_subscription` / `send_update_for_track_subscription` に検証済み parameters を渡すようにした。
- `src/session/namespace/subscribe_namespace.rs` / `track_subscription.rs`: 送信時に作成時と同じ条件で overlap を
  送前検査し (確定待ちを含む実効 prefix で比較、Terminated は対象外)、確定待ちを登録する。REQUEST_OK の
  Established 分岐で先頭を適用し、対応する確定待ちが無い REQUEST_OK は PROTOCOL_VIOLATION とする。
  REQUEST_ERROR / bidi 終端 / forget で確定待ちを破棄する。作成時の overlap 検査も実効 prefix 比較に変更した。
- `src/session/types.rs`: `prefix` の doc を役割別 (subscriber は REQUEST_OK で確定した適用済み、publisher は受理した REQUEST_UPDATE を反映) に更新した。
- `tests/test_session/namespace/subscribe_namespace.rs` / `track_subscription.rs` に送信側 prefix 更新の回帰テストを追加した。
  送信直後は旧 prefix / REQUEST_OK 後は新 prefix、in-flight NAMESPACE / NAMESPACE_DONE、確定待ち中の新旧 prefix PUBLISH の紐付け、
  連続更新と REQUEST_OK の対応、ローカル overlap 拒否と確定待ち非残留、REQUEST_ERROR / bidi 終端、
  余剰 REQUEST_OK、Terminated 購読の除外を検証する。
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した。

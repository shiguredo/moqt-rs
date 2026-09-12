# REQUEST_UPDATE の TRACK_NAMESPACE_PREFIX 更新で予約名前空間を再検証する

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-request-update-prefix-reserved-namespace
- Polished: 2026-09-13

## 目的

REQUEST_UPDATE で `TRACK_NAMESPACE_PREFIX` を変更したとき、予約名前空間 (`.` と `.session`) を再検証し、初回の SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS と同じ MUST を更新経路でも満たす。確立後に prefix を予約名前空間へ変更できる現状は、初回拒否を迂回する経路になっている。

## 現状

初回の購読は予約名前空間を拒否するが、確立後の prefix 更新は再検証しない。

- `handle_peer_subscribe_namespace` (SUBSCRIBE_NAMESPACE) と `handle_peer_subscribe_tracks` (SUBSCRIBE_TRACKS) は、`.` を `"reserved single-period namespace"`、`.session` をそれぞれ `"session-level namespace does not exist"` / `"session-level track does not exist"` として `DOES_NOT_EXIST` の REQUEST_ERROR で拒否する。
- `handle_update_for_namespace_subscription` / `handle_update_for_track_subscription` は role / state と prefix overlap を検証するが、予約名前空間の再検証はなく `parameters.track_namespace_prefix()` をそのまま `entry.prefix` へ書き込む。なお track 側のハンドラは FORWARD の値域検証 (値域外は MUST close) と Range Filter の適用も行う。
- 送信側の `send_request_update` は parameter scope を検証して `send_update_for_namespace_subscription` / `send_update_for_track_subscription` に委譲するが、prefix の予約名前空間検証はない。`NAMESPACE_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS` / `TRACK_SUBSCRIPTION_UPDATE_ALLOWED_PARAMS` に `TRACK_NAMESPACE_PREFIX` が含まれる。
- 初回の送信 API (`send_subscribe_namespace` / `send_subscribe_tracks`) は `.` のみを拒否し、`.session` のローカル検証は持たない (受信側の初回拒否のみが `.session` を扱う)。

再現手順:

1. `["example"]` prefix で SUBSCRIBE_NAMESPACE (または SUBSCRIBE_TRACKS) を確立する
2. `TRACK_NAMESPACE_PREFIX = [".session"]` (または `["."]`) の REQUEST_UPDATE を送る
3. overlap する他購読がなければ prefix が更新され、`RequestUpdateReceived` が Application へ届く

根拠 (draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces)):

> "An endpoint that receives a request for an unrecognized session-level track or namespace MUST reject it with REQUEST_ERROR using error code DOES_NOT_EXIST rather than passing it to the Application."

根拠 (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces)):

> "A Track Namespace whose first field is exactly . [...] is reserved and MUST NOT be used for any purpose"

## 設計方針

- 受信側は `handle_update_for_*` の role / state 検証直後、FORWARD 値域検証より前に予約名前空間検証を置く (予約名前空間の検証を優先する。open issue 0056 が扱う予約名前空間と他検証の優先順位と揃える)。`.` / `.session` は `DOES_NOT_EXIST` の REQUEST_ERROR で拒否し、prefix を反映しない。拒否時の bidi stream の扱いは既存の prefix overlap 拒否と揃える (`emit_request_error` による REQUEST_ERROR +
  FIN、state は Established のまま維持)。
- 送信側も受信側と同じ条件 (`.` / `.session`) でローカル拒否する。検証は `send_update_for_namespace_subscription` / `send_update_for_track_subscription` の既存ローカル overlap 検査と同じ位置 (確定待ち `push_pending_prefix_update` より前) に追加する。0017 (送信側の確定待ちキューとローカル overlap 検査) は完了済みで、本 issue はその構造の上に検証を追加する。初回の送信 API
  (`send_subscribe_namespace` / `send_subscribe_tracks`) の変更は本 issue の対象外とする。
- 既存テスト (`tests/test_session/namespace/subscribe_namespace.rs` など) には `["newprefix"]` への更新テストはあるが予約名前空間への更新テストはないため、回帰テストを追加する。

## 完了条件

- 確立後に `TRACK_NAMESPACE_PREFIX` を `.` / `.session` へ変更する REQUEST_UPDATE が拒否され、prefix が旧値のままであること
- 拒否時に REQUEST_ERROR が送られ、セッションが閉じないこと
- 送信側の更新 API (`send_request_update` 経由) が `.` / `.session` への prefix 更新を拒否すること (初回送信 API は対象外)
- 受信側・送信側の回帰テストが `tests/test_session/namespace/` に追加され、`cargo test --workspace` が通ること

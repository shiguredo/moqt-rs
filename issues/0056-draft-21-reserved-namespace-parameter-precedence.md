# 予約名前空間の拒否とパラメータ値域 MUST の優先順位を明確にする

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/update-reserved-namespace-parameter-precedence
- Polished: 2026-09-13

## 目的

同一メッセージが「予約名前空間の拒否 (REQUEST_ERROR)」と「パラメータ値域の MUST close」の両方に該当するときの優先順位を「予約名前空間優先」に確定し、選んだ方針をコードコメントとテストで固定する。draft は優先順位を規定していないため、現状の挙動が仕様上の意図なのか実装順序の副産物なのかがコードから読み取れない。

## 現状

各受信ハンドラは `.` / `.session` の拒否を一部の値域検証より先に行うが、ハンドラごとに順序が異なる。

- `handle_peer_subscribe`: `.` → `.session` → FORWARD → INCLUDE_PROPERTIES → GROUP_ORDER の順。GROUP_ORDER / INCLUDE_PROPERTIES の値域 MUST は予約名前空間拒否の後に評価される。
- `handle_peer_publish`: `.` → `.session` → FORWARD → GROUP_ORDER の順。GROUP_ORDER の値域 MUST は後。
- `handle_peer_subscribe_tracks`: `.` → `.session` → GROUP_ORDER → FORWARD → INCLUDE_PROPERTIES の順。
- `handle_peer_track_status`: `.` → `.session` → INCLUDE_PROPERTIES の順。
- `handle_peer_fetch`: LOCATION_FILTER の decode (StartGroup + EndGroupDelta が 2^64 - 1 を超えると MUST close) → `.` → `.session` → GROUP_ORDER → INCLUDE_PROPERTIES の順。fetch だけは値域検証が予約名前空間拒否より先にある。
- wire 経路では FORWARD / GROUP_ORDER / INCLUDE_PROPERTIES の値域外は decode 層 (`validate_uint8_param_value`) がハンドラ到達前に拒否する。両者が競合するのは `MessageParameters::push` で値域外パラメータを構築して `recv_request` へ渡す API 経路のみ (`push` は値域を検証しない)。
- 値域外の GROUP_ORDER / INCLUDE_PROPERTIES は「MUST close the session with PROTOCOL_VIOLATION」であり、`.session` の未認識リクエストは「MUST reject it with REQUEST_ERROR using error code DOES_NOT_EXIST」、`.` のリクエストは「MUST reject ... with error code DOES_NOT_EXIST」である。

根拠 (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter)):

> "If an endpoint receives a value outside this range, it MUST close the session with PROTOCOL_VIOLATION."

根拠 (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter)):

> "If an endpoint receives a value outside this range, it MUST close the session with PROTOCOL_VIOLATION."

根拠 (draft-ietf-moq-transport-21 §6.5 (Session-Level Tracks and Namespaces)):

> "An endpoint that receives a request for an unrecognized session-level track or namespace MUST reject it with REQUEST_ERROR using error code DOES_NOT_EXIST rather than passing it to the Application."

根拠 (draft-ietf-moq-transport-21 §2.4.2 (Reserved Namespaces)):

> "A Track Namespace whose first field is exactly . (a single period, 0x2e) is reserved and MUST NOT be used for any purpose; endpoints MUST NOT publish tracks or namespaces under it and MUST reject requests referencing it with DOES_NOT_EXIST."

## 設計方針

- 採用案: 予約名前空間の拒否を優先する。`.` / `.session` は request の宛先自体が仕様で不存在 (§2.4.2 / §6.5) であり、パラメータを解釈する前に拒否できる。値域 MUST は wire 経路では decode 層が先に発火するため、この選択で MUST close が wire 上で落ちることはなく、API 経路でのみ値域 MUST が落ちることをトレードオフとしてコメントに残す。
- 対象ハンドラは `handle_peer_subscribe` / `handle_peer_publish` / `handle_peer_subscribe_tracks` / `handle_peer_track_status` / `handle_peer_fetch` の 5 つとする。`handle_peer_subscribe_namespace` / `handle_peer_publish_namespace` は AUTHORIZATION_TOKEN 以外の値域パラメータを許可しないため対象外。
- `handle_peer_fetch` は `.` / `.session` の拒否を LOCATION_FILTER の decode より前へ移し、他の 4 ハンドラと同じ「予約名前空間優先」に揃える。これにより API 経路 (手組みメッセージ) で `.session` かつ LOCATION_FILTER 不正な FETCH は close ではなく `DOES_NOT_EXIST` の REQUEST_ERROR になる (意図した挙動変更としてテストで固定する。wire 経路では decode 層が先に拒否するため到達しない)。
- 順序が既に予約名前空間優先になっている 4 ハンドラは順序を変更せず、予約名前空間拒否の箇所に「draft は優先順位を規定しないが、予約名前空間を優先する意図した選択である。値域 MUST は wire 経路では decode 層が発火する」旨のコメントを根拠付きで追加する。
- 却下した案: 「値域 MUST 優先」は wire 経路で decode が先に拒否するため実利がなく、`.session` の拒否が値域の組合せで REQUEST_ERROR から close に変わる根拠がない。「両方を満たす」(REQUEST_ERROR + close) は close 後に応答が peer へ届く保証がなく (RFC 9000 の接続終了時の取り扱い)、応答の検証もできないため採用しない。
- open issue 0053 が扱う REQUEST_UPDATE の prefix 予約名前空間検証も同じ「予約名前空間優先」に従う。0053 は本 issue の採用案を前提に挿入位置を決めているため、実装時に方針を揃える。

## 完了条件

- 採用案「予約名前空間優先」が対象 5 ハンドラの予約名前空間拒否箇所に根拠付きで明記されていること
- `handle_peer_fetch` で `.` / `.session` が LOCATION_FILTER の decode より前に拒否されること
- 各ハンドラで `.` / `.session` と値域外 GROUP_ORDER / INCLUDE_PROPERTIES を組み合わせた手組みメッセージを `recv_request` に渡したとき、`DOES_NOT_EXIST` の REQUEST_ERROR が送られセッションが閉じないことがテストで固定されていること
- `handle_peer_fetch` で `.session` と LOCATION_FILTER 不正の組合せが `DOES_NOT_EXIST` になることがテストで固定されていること
- 0053 の prefix 更新検証が同じ優先順位で実装できる形になっていること (0053 側の変更は 0053 で行う)
- 既存テストがすべて通ること

# 予約名前空間の拒否とパラメータ値域 MUST の優先順位を明確にする

- Created: 2026-09-11
- Completed: {YYYY-MM-DD}
- Branch: feature/update-reserved-namespace-parameter-precedence
- Polished: {YYYY-MM-DD}

## 目的

同一メッセージが「予約名前空間の拒否 (REQUEST_ERROR)」と「パラメータ値域の MUST close」の両方に該当するときの優先順位を明確にし、選んだ方針をコードコメントとテストで固定する。draft は優先順位を規定していないため、現状の挙動が仕様上の意図なのか実装順序の副産物なのかがコードから読み取れない。

## 現状

予約名前空間の判定がパラメータ値域の検証より先にあり、値域外パラメータを受信してもセッションが閉じない。

- `handle_peer_fetch` は `.session` 判定 → LOCATION_FILTER → GROUP_ORDER 値域 → INCLUDE_PROPERTIES 値域の順で評価し、`.session` 拒否は値域検証より前に return する。
- `handle_peer_track_status` も `.session` 拒否が INCLUDE_PROPERTIES の値域検証より前にある。
- `handle_peer_subscribe` も `.session` 拒否が GROUP_ORDER 検証より前にある。
- 値域外の GROUP_ORDER / INCLUDE_PROPERTIES は「MUST close the session with PROTOCOL_VIOLATION」であり、`.session` の未認識リクエストは「MUST reject it with REQUEST_ERROR using error code DOES_NOT_EXIST」である。

根拠 (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter)):

> "If an endpoint receives a value outside this range, it MUST close the session with PROTOCOL_VIOLATION."

根拠 (draft-ietf-moq-transport-21 §9.20.22 (INCLUDE_PROPERTIES Parameter)):

> "If an endpoint receives a value outside this range, it MUST close the session with PROTOCOL_VIOLATION."

根拠 (draft-ietf-moq-transport-21 §6.5):

> "An endpoint that receives a request for an unrecognized session-level track or namespace MUST reject it with REQUEST_ERROR using error code DOES_NOT_EXIST rather than passing it to the Application."

## 設計方針

優先順位の決定を本 issue の成果物とし、次のいずれかを選ぶ。

- 予約名前空間の拒否を優先する (現状): 仕様が優先順位を規定しないことをコメントに明記し、現状の順序が意図した選択であることを残す。値域 MUST が発火しないケースがあることをトレードオフとして記録する。
- 値域 MUST を優先する: MUST close 系の検証を名前空間判定より前に移す。全ハンドラと既存テストへの影響を洗い出し、テストを追従させる。
- 両方を満たす: REQUEST_ERROR を送ったうえでセッションを閉じる。応答の順序と wire 上の整合を検証する。

推奨は現状の予約名前空間優先を明文化する案とする (予約名前空間は request の宛先自体が仕様で定義された不存在であり、パラメータから独立して拒否できるため)。ただし MUST close を落とす判断なので、採用案の根拠をコメントに残す。

## 完了条件

- 優先順位の採用案が決定し、対象ハンドラのコメントに根拠付きで明記されていること
- 採用案どおりの挙動がテストで固定されていること
- 値域検証順序の変更を伴う場合は、変更したハンドラの既存テストが追従し `cargo test --workspace` が通ること

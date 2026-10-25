# FETCH の REQUEST_UPDATE_OK に LARGEST_OBJECT を自動補完する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-10-25

## 目的

draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter) は "If Objects have been published on this Track the Publisher
MUST include this parameter." と MUST を課す。LARGEST_OBJECT は SUBSCRIBE_OK / PUBLISH / REQUEST_UPDATE_OK / TRACK_STATUS_OK /
PUBLISH_STATE_NOTIFY に載りうる。session 層は SUBSCRIBE_OK / PUBLISH / subscription の REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY では Track
単位の最大 Location を自動補完しているが、FETCH の REQUEST_UPDATE_OK と TRACK_STATUS_OK は補完しない
(TRACK_STATUS_OK は open の 0304 で扱う)。

## 現状

- `src/session/core.rs` の `Session::send_request_ok` は Fetch 分岐で `send_ok_for_fetch(request_id)` を呼び、parameters を渡さない。
- `src/session/fetch.rs` の `send_ok_for_fetch` は parameters を受け取らず、LARGEST_OBJECT の注入も行わない。
- subscription 側は `src/session/subscription/dispatch.rs` の `send_ok_for_subscription` が
  `update_largest_object_in_parameters` を呼び、`Session::publisher_track_largest` で Track 最大を求めて既存値との大きい方を載せている。
  `send_subscribe_ok` / `send_publish_state_notify` も同じ helper を使う。
- `Session::publisher_track_largest` は publisher 役 subscription の `send_subgroup_object` / `send_object_datagram` の送信で更新される。
  `send_fetch_object` は現状 stream id しか受け取らず、FETCH 応答で送った Object は記録されない。
- `docs/moqt.md` の Message Parameters 節は「TRACK_STATUS_OK と FETCH の REQUEST_UPDATE_OK は未対応」と明記している。
  TRACK_STATUS_OK 側は open の 0304 で扱っており、本 issue は FETCH の REQUEST_UPDATE_OK のみを対象とする。

## 設計方針

- `send_ok_for_fetch` を `&mut MessageParameters` を受け取る形に変更し、fetch が保持する `track_namespace` / `track_name` から
  `Session::publisher_track_largest` を引いて、既存の `update_largest_object_in_parameters` で LARGEST_OBJECT を補完する。
  既存 helper は「利用者指定値と観測最大の大きい方」を載せる (指定値を下げない) ため、0304 の TRACK_STATUS_OK 側と同じ規則になる。
  自動補完の情報源は publish 経路の送信記録に限られ、FETCH 応答で送った Object は含まれない。この制約は doc に明記する
  (FETCH 応答でしか Object を送らない Track では MUST を満たせない既知の制約)。
- `Session::send_request_ok` の Fetch 分岐で parameters を渡す。
- `REQUEST_UPDATE_OK_ALLOWED_PARAMS` のスコープ検証は従来どおり `send_request_ok` 側で行い、順序を変えない。
- `docs/moqt.md` の LARGEST_OBJECT の説明を、FETCH の REQUEST_UPDATE_OK も自動補完する形に更新する。
- `tests/test_session/fetch/request_update.rs` に、publisher 役 subscription と `send_subgroup_object` で publish 観測を作った Track への
  FETCH REQUEST_UPDATE_OK に LARGEST_OBJECT が自動で載ること、publish 観測がない Track では自動補完されないこと、
  利用者が明示指定した値が観測最大より大きい場合はそのまま、小さい場合は観測最大へ引き上げられる (指定値を下げない) ことを追加する。

## 完了条件

- publisher 役として publish 済み Track への FETCH の REQUEST_UPDATE_OK に、Track 最大の LARGEST_OBJECT が自動で載ること。
- publish 観測がない Track への FETCH の REQUEST_UPDATE_OK には LARGEST_OBJECT が自動補完されないこと
  (利用者が明示した値はそのまま載る)。
- 利用者が明示指定した LARGEST_OBJECT は、観測最大より大きければそのまま、小さければ観測最大へ引き上げられる
  (指定値を下げない。`update_largest_object_in_parameters` と同じ規則)。
- `docs/moqt.md` の記述が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

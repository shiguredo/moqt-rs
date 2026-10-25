# FETCH の REQUEST_UPDATE_OK に LARGEST_OBJECT を自動補完する

- Created: 2026-10-25
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter) は "If Objects have been published on this Track the Publisher
MUST include this parameter." と MUST を課す。LARGEST_OBJECT は SUBSCRIBE_OK / PUBLISH / REQUEST_UPDATE_OK / TRACK_STATUS_OK /
PUBLISH_STATE_NOTIFY に載りうる。session 層は SUBSCRIBE_OK / PUBLISH / subscription の REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY では Track
単位の最大 Location を自動補完しているが、FETCH の REQUEST_UPDATE_OK だけは補完しない。

## 現状

- `src/session/core.rs` の `Session::send_request_ok` は Fetch 分岐で `send_ok_for_fetch(request_id)` を呼び、parameters を渡さない。
- `src/session/fetch.rs` の `send_ok_for_fetch` は parameters を受け取らず、LARGEST_OBJECT の注入も行わない。
- subscription 側は `src/session/subscription/send.rs` の `send_ok_for_subscription` が `Session::publisher_track_largest` で Track
  最大を求めて補完している。
- `docs/moqt.md` の Message Parameters 節は「TRACK_STATUS_OK と FETCH の REQUEST_UPDATE_OK は未対応」と明記している。TRACK_STATUS_OK 側は別 issue
  で扱われており、FETCH 側は未起票である。

## 設計方針

- `send_ok_for_fetch` を `&mut MessageParameters` を受け取る形に変更し、fetch が保持する `track_namespace` / `track_name` から
  `Session::publisher_track_largest` を引いて、LARGEST_OBJECT が未指定かつ観測済みの最大 Location がある場合に挿入する。
- `Session::send_request_ok` の Fetch 分岐で parameters を渡す。
- `REQUEST_UPDATE_OK_ALLOWED_PARAMS` のスコープ検証は従来どおり `send_request_ok` 側で行い、順序を変えない。
- `docs/moqt.md` の LARGEST_OBJECT の説明を、FETCH の REQUEST_UPDATE_OK も自動補完する形に更新する。
- `tests/test_session/fetch/request_update.rs` に、publish 済み Track への FETCH REQUEST_UPDATE_OK に LARGEST_OBJECT が自動で載ること、publish
  観測がない Track では載らないこと、利用者が明示指定した値は上書きしないことを追加する。

## 完了条件

- publisher 役として publish 済み Track への FETCH の REQUEST_UPDATE_OK に、Track 最大の LARGEST_OBJECT が自動で載ること。
- publish 観測がない Track への FETCH の REQUEST_UPDATE_OK には LARGEST_OBJECT が載らないこと。
- 利用者が明示指定した LARGEST_OBJECT を上書きしないこと。
- `docs/moqt.md` の記述が実装と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

# TRACK_STATUS_OK に LARGEST_OBJECT を自動補完する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-track-status-largest-object
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §9.20.17 (LARGEST OBJECT Parameter) は "If Objects have been published on this Track the Publisher MUST include this parameter." と MUST を課す。SUBSCRIBE_OK / PUBLISH / subscription の REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY は publish 済み最大 Location を自動補完しているが、
TRACK_STATUS_OK だけが parameters への自動補完を行わない。また `send_ok_for_track_status` は `TrackStatusResponse::Ok.largest_location` を `None` のまま記録する。そのため、publish 済み Track でアプリが明示的に LARGEST_OBJECT を渡さない限り MUST 違反になる。

## 現状

- `src/session/track_status.rs` の `send_ok_for_track_status` は応答済み記録と `INCLUDE_PROPERTIES=0` の空化を行い、`publisher_track_largest` を使った LARGEST_OBJECT の注入を行わない。シグネチャも `parameters` を受け取らない。`entry.response` には `TrackStatusResponse::Ok { largest_location: None }` を記録する。
- `src/session/core.rs` の `send_request_ok` の TRACK_STATUS 分岐も parameters への注入を行わない。`TRACK_STATUS_OK_ALLOWED_PARAMS` は LARGEST_OBJECT を許可しているため、アプリが明示すれば wire には載せられるが、自動補完はされない。
- FETCH の REQUEST_UPDATE_OK は `src/session/fetch.rs` の `send_ok_for_fetch` が parameters を受け取らず、LARGEST_OBJECT の自動補完を行わない。本 issue は TRACK_STATUS_OK の経路だけを対象とし、FETCH の REQUEST_UPDATE_OK は別途扱う。
- closed 0015 / 0078 で同種の MUST に対応した履歴があるが、現行の `src/session/track_status.rs` には自動補完が無く、0015 で実装した自動補完が失われている (回帰)。
- `tests/test_session/track_status.rs` のコメントは LARGEST_OBJECT を「任意」としており、仕様と逆である。
- 影響: publish 済み Track への TRACK_STATUS 応答が、アプリが明示しない限り MUST 違反になる。`publisher_track_largest` は `pub(crate)` で公開 API ではないため、アプリが自身で最新 Location を管理するか、公開 API の subscription 群から同じ集約を実装する必要があり、上記の他 context の自動補完と非対称である。

## 設計方針

- `send_ok_for_track_status` に `parameters: &mut MessageParameters` を渡し、`publisher_track_largest` が返す Location があれば `update_largest_object_in_parameters` で LARGEST_OBJECT を載せる。
- `send_request_ok` の TRACK_STATUS 分岐から `&mut parameters` を渡し、`send_ok_for_track_status` 内でアプリが明示した値より大きい場合だけ置き換える方針を他 context と揃える。
- 最終的に parameters に載った LARGEST_OBJECT を `TrackStatusResponse::Ok.largest_location` にも反映する (`TrackStatusResponse` の doc「REQUEST_OK 受信 / 送信済み (LARGEST_OBJECT parameter があれば保持する)」に合わせる)。現在の `send_ok_for_track_status` のコメント「自側が送る応答では Largest Location を保持しない」はこの doc と矛盾するため、doc に合わせて最終値を保持する。
- Object 未 publish の Track では自動補完しない。アプリが明示した LARGEST_OBJECT は、その Track で Object を publish していなくても上書き・削除しない (他 context の「アプリ指定値との max」と同じ)。
- `publisher_track_largest` は publisher 役 subscription が保持する値を使うため、対象 Track の publisher 役 subscription をすべて `forget_subscription` した後は自動補完できない。Track 単位で largest を永続保持する改善は本 issue のスコープ外とする。

## 完了条件

- publisher 役 subscription が保持されている publish 済み Track への TRACK_STATUS_OK に LARGEST_OBJECT が自動補完されること。
- Object 未 publish の Track では LARGEST_OBJECT が自動補完されないこと。
- アプリが明示した LARGEST_OBJECT と自動補完値の大きい方が載ること (アプリ指定値を下げない)。
- 送信した TRACK_STATUS_OK の最終 LARGEST_OBJECT が `TrackStatusResponse::Ok.largest_location` に反映されること。
- テストのコメントが仕様に一致し、上記を単体テストで固定すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

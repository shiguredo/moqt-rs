# TRACK_STATUS_OK に LARGEST_OBJECT を自動補完する

- Created: 2026-10-19
- Completed: 2026-11-11
- Branch: develop (CODEBASE.md の規約により develop に直接コミットする)
- Polished: 2026-10-19
- Refreshed: 2026-11-04

## 目的

draft-ietf-moq-transport-23 §9.19.17 (LARGEST OBJECT Parameter) は、このパラメータを SUBSCRIBE_OK / PUBLISH / REQUEST_UPDATE_OK /
TRACK_STATUS_OK / FETCH_OK / PUBLISH_STATE_NOTIFY に含められ、送信側が Track で Object を publish または受信していれば
"it MUST include this parameter in the above messages it sends" と MUST を課す (FETCH_OK は Largest Object が要求範囲内の場合のみ)。
SUBSCRIBE_OK / PUBLISH / subscription の REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY は publish 済み最大 Location を自動補完しているが、
TRACK_STATUS_OK だけが parameters への自動補完を行わない。また `send_ok_for_track_status` は `TrackStatusResponse::Ok.largest_location` を `None` のまま記録する。そのため、publish 済み Track でアプリが明示的に LARGEST_OBJECT を渡さない限り MUST 違反になる。

## 現状

- `src/session/track_status.rs` の `send_ok_for_track_status` は応答済み記録と `INCLUDE_PROPERTIES=0` の空化を行い、`publisher_track_largest` を使った LARGEST_OBJECT の注入を行わない。シグネチャも `parameters` を受け取らない。`entry.response` には `TrackStatusResponse::Ok { largest_location: None }` を記録する。
- `src/session/core.rs` の `send_request_ok` の TRACK_STATUS 分岐も parameters への注入を行わない。`TRACK_STATUS_OK_ALLOWED_PARAMS` は LARGEST_OBJECT を許可しているため、アプリが明示すれば wire には載せられるが、自動補完はされない。
- FETCH の REQUEST_UPDATE_OK は `src/session/fetch.rs` の `send_ok_for_fetch` が `&mut MessageParameters` を受け取り、`publisher_track_largest` と `update_largest_object_in_parameters` で自動補完している。TRACK_STATUS_OK 側だけが非対称である。
- closed 0015 / 0078 で同種の MUST に対応した履歴があるが、現行の `src/session/track_status.rs` には自動補完が無く、0015 で実装した自動補完が失われている (回帰)。
- `tests/test_session/track_status.rs` のコメントは LARGEST_OBJECT を「任意」としており、仕様と逆である。
- 影響: publish 済み Track への TRACK_STATUS 応答が、アプリが明示しない限り MUST 違反になる。`publisher_track_largest` は `pub(crate)` で公開 API ではないため、アプリが自身で最新 Location を管理するか、公開 API の subscription 群から同じ集約を実装する必要があり、上記の他 context の自動補完と非対称である。

## 設計方針

- `send_ok_for_track_status` に `parameters: &mut MessageParameters` を渡し、`publisher_track_largest` が返す Location があれば `update_largest_object_in_parameters` で LARGEST_OBJECT を載せる。
- `send_request_ok` の TRACK_STATUS 分岐から `&mut parameters` を渡し、`send_ok_for_track_status` 内でアプリが明示した値より大きい場合だけ置き換える方針を他 context と揃える。
- 最終的に parameters に載った LARGEST_OBJECT を `TrackStatusResponse::Ok.largest_location` にも反映する (`TrackStatusResponse` の doc「REQUEST_OK 受信 / 送信済み (LARGEST_OBJECT parameter があれば保持する)」に合わせる)。現在の `send_ok_for_track_status` のコメント「自側が送る応答では Largest Location を保持しない」はこの doc と矛盾するため、doc に合わせて最終値を保持する。
- Object 未 publish の Track では自動補完しない。アプリが明示した LARGEST_OBJECT は、その Track で Object を publish していなくても上書き・削除しない (他 context の「アプリ指定値との max」と同じ)。
- `publisher_track_largest` は publisher 役 subscription が保持する値と、Track 単位で保持した自側 publish 済み最大の大きい方を返す。Track 単位の保持は 0279 で導入済みのため、対象 Track の publisher 役 subscription をすべて `forget_subscription` した後も自動補完できる。

## 完了条件

- publisher 役 subscription が保持されている publish 済み Track への TRACK_STATUS_OK に LARGEST_OBJECT が自動補完されること。
- Object 未 publish の Track では LARGEST_OBJECT が自動補完されないこと。
- アプリが明示した LARGEST_OBJECT と自動補完値の大きい方が載ること (アプリ指定値を下げない)。
- 送信した TRACK_STATUS_OK の最終 LARGEST_OBJECT が `TrackStatusResponse::Ok.largest_location` に反映されること。
- テストのコメントが仕様に一致し、上記を単体テストで固定すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `src/session/track_status.rs` の `send_ok_for_track_status` を `parameters: &mut MessageParameters` を受け取る形に変え、`Session::publisher_track_largest` が返す Track 最大を `update_largest_object_in_parameters` で自動補完するようにした。同関数はアプリ指定値との大きい方を採るため、アプリが明示した値を下げない。Object 未 publish の Track では何もしない (アプリ指定値はそのまま残す)。
- 検証とフィールドの取り出しは entry の不変借用で行い、`publisher_track_largest` の呼び出し後に可変借用を取り直して `entry.response` を設定する。`publisher_track_largest` も `self` を不変借用するため、可変借用を先に取ると両立しない。
- `entry.response` を `TrackStatusResponse::Ok { largest_location: <最終値>, expires_ms: None }` に変更した。従来は `largest_location: None` で「自側が送る応答では Largest Location を保持しない」としていたが、同 enum の doc「REQUEST_OK 受信 / 送信済み (LARGEST_OBJECT parameter があれば保持する)」と矛盾していたため doc に合わせた。EXPIRES は従来どおり `None` のまま。
- `src/session/core.rs` の `send_request_ok` の TRACK_STATUS 分岐から `&mut parameters` を渡すようにした。
- `docs/moqt.md` の Largest Object 節を、TRACK_STATUS_OK も自動補完する形に更新した (「TRACK_STATUS_OK は未対応」を削除)。
- `tests/test_session/track_status.rs` に `largest_object` モジュールを追加した。
  - `track_status_ok_fills_track_largest_object`: publish 済み Track (group 3 / object 7) への TRACK_STATUS_OK に LARGEST_OBJECT が載り、送信側 entry の `response` にも `Location { 3, 7 }` が反映されること。受信側でも同じ値になること。
  - `track_status_ok_omits_largest_object_without_published_object`: Object 未 publish の Track では補完されず `response` も `None` のままであること。
  - `track_status_ok_keeps_larger_app_supplied_largest_object`: アプリ指定値が観測最大より大きければそのまま、小さければ観測最大へ引き上げ、引き上げ後の値が entry に残ること。
  - 既存の `track_status_completes_on_request_ok` のコメントにあった「LARGEST_OBJECT は任意で載る」を仕様 (§9.19.17 の MUST) に合わせて修正した。
- 検証: `cargo test -p shiguredo_moqt --test test_session` (1104 件) / `cargo clippy -p shiguredo_moqt --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通る。

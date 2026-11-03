# SUBSCRIBE_TRACKS の許可パラメータを明示リストへ限定する

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.6.2、§9.17](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/message.rs` の `SUBSCRIBE_TRACKS_ALLOWED_PARAMS` は draft 22 の記述の食い違いを補うため SUBSCRIBE の許可集合との和集合になっており、RENDEZVOUS_TIMEOUT / NEW_GROUP_REQUEST を許している。
`subscribe_tracks_allowed_params_covers_subscribe_params` も包括許可を固定している。

## 設計方針

draft 23 の明示リストを採用し、RENDEZVOUS_TIMEOUT と NEW_GROUP_REQUEST を除外する。
OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT / SUBSCRIBER_PRIORITY / LOCATION_FILTER / FILL_PARAMETERS は明示的に残す。
`Session::recv_request` の未対応要求の検証は維持し、正当な要求には NOT_SUPPORTED を返す。
namespace の購読・告知機能そのものは追加しない。

## 完了条件

- 列挙された各パラメータで正当な SUBSCRIBE_TRACKS を構築でき、NOT_SUPPORTED 応答になること。
- 除外された 2 型は INVALID_PARAMETER で拒否され、認証 cache や要求状態へ副作用を残さないこと。
- 旧和集合テストを明示集合の受理・拒否テストに置き換えること。

## 解決方法

`src/message.rs` の `SUBSCRIBE_TRACKS_ALLOWED_PARAMS` を draft-ietf-moq-transport-23 §9.17 (SUBSCRIBE_TRACKS) の明示リスト 14 種へ置き換えた。
draft 22 の §3.6.2 の包括許可を根拠にしていた SUBSCRIBE の許可集合との和集合を廃止し、RENDEZVOUS_TIMEOUT と NEW_GROUP_REQUEST を除外した。
§3.6.2 (Parameters on SUBSCRIBE_TRACKS) が「単一 Track の SUBSCRIBE にだけ意味を持つ RENDEZVOUS_TIMEOUT と NEW_GROUP_REQUEST は SUBSCRIBE_TRACKS に載せられない」と定めているためである。
OBJECT_DELIVERY_TIMEOUT / SUBGROUP_DELIVERY_TIMEOUT / SUBSCRIBER_PRIORITY / LOCATION_FILTER / FILL_PARAMETERS も §9.17 の列挙に含まれるため明示的に残した。
`Session::recv_request` の未対応要求の検証は変更せず、許可されたパラメータのみを持つ要求は NOT_SUPPORTED、除外した 2 型は §9.19.1 (Parameter Scope) の MUST により INVALID_PARAMETER でセッションを閉じる動作を維持した。
namespace の購読・告知機能そのものは追加していない。
`docs/moqt.md` の許可集合と `RENDEZVOUS_TIMEOUT` / `NEW_GROUP_REQUEST` の記述も明示リストへ追随させた。

## 完了条件の検証

- 列挙された各パラメータで正当な SUBSCRIBE_TRACKS を構築でき、NOT_SUPPORTED 応答になること。
  - `tests/test_session/request_stream.rs` の `subscribe_tracks_accepts_each_parameter_from_message_definition` が 14 種を 1 種ずつ wire encode / decode 経由で送信し、セッション維持と NOT_SUPPORTED 応答を検証する。
  - `subscribe_tracks_accepts_all_parameters_together` が 14 種を同時に載せた要求を検証する。
- 除外された 2 型は INVALID_PARAMETER で拒否され、認証 cache や要求状態へ副作用を残さないこと。
  - `subscribe_tracks_rejects_single_track_only_parameters` が RENDEZVOUS_TIMEOUT と
    NEW_GROUP_REQUEST を個別に検証し、INVALID_PARAMETER の CloseSession のみが発行されること、
    REQUEST_ERROR を送らないこと、同じメッセージの REGISTER が token cache に残らないこと、
    subscription / fetch / track status の要求状態を作らないことを確認する。
- 旧和集合テストを明示集合の受理・拒否テストに置き換えること。
  - `subscribe_tracks_allowed_params_covers_subscribe_params` を削除し、
    `subscribe_tracks_allowed_params_matches_spec_list` と
    `subscribe_tracks_allowed_params_excludes_subscribe_only_params` に置き換えた。
    前者は §9.17 の 14 種との集合一致、後者は 2 型が SUBSCRIBE にだけ含まれることを検証する。
- 検証コマンド: `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` がすべて成功した。

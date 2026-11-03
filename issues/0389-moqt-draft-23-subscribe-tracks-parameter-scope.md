# SUBSCRIBE_TRACKS の許可パラメータを明示リストへ限定する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-subscribe-tracks-parameter-scope
- Polished: {YYYY-MM-DD}

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

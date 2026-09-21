# Redirect の Full Track Name 長検証が欠落している

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-redirect-full-track-name-length
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §8.7 (Track Namespace Structure) は Full Track Name の最大長を 4,096 バイトとし、超過を受信したら PROTOCOL_VIOLATION でセッションを閉じる MUST を定める。

> The maximum total length of a Full Track Name is 4,096 bytes.
> The length of a Full Track Name is computed as the sum of the Track Namespace Field Length fields and the Track Name Length field.
> The length of a Track Namespace is the sum of the Track Namespace Field Length fields.
> If an endpoint receives a Track Namespace or a Full Track Name exceeding 4,096 bytes, it MUST close the session with a PROTOCOL_VIOLATION.

§9.4.1 (Redirect Structure) の Redirect は Track Namespace と Track Name を持ち、これらは「Redirect target」と呼ばれる。現状は合計長の検証が抜けているため、上限を超える Redirect を送信・受理しうる。

## 現状

- `src/message/common.rs` の `validate_full_track_name` は namespace 長 + track name 長を検査する。呼び出し元は `src/message.rs` の `Subscribe` / `Publish` / `Fetch` / `TrackStatus` の `encode_message_body` と `decode_message_body` だけである
- `src/message.rs` の `Redirect::encode_to` と `Redirect::decode_from` は `validate_full_track_name` を呼ばない
- namespace 単体の上限は `TrackNamespace::encode_to` / `TrackNamespace::decode_from` が検査する。このため namespace 4,096 バイト + track name 1 バイトのような合計 4,097 バイトの Redirect が encode / decode とも通る
- `src/session/core.rs` の `handle_peer_request_error` は §9.4.1 の Connect URI 長 (server が非ゼロ長を受信した場合の拒否) だけを検証し、Redirect target の合計長は検証しない
- `tests/test_message.rs` の Redirect 系テストは欠落・余剰バイト・error_code と Redirect の present 整合のみを対象にしており、長さ上限のテストは無い

## 設計方針

- Redirect の Track Namespace と Track Name は §9.4.1 で「Redirect target」と総称される。

  > Track Namespace and Track Name: The Track Namespace and Track Name to use for the redirected request, together referred to as the Redirect target.

  §2.4.1 (Track Naming) は次のとおり定義しており、Redirect target は Full Track Name そのものである。

  > In MOQT, every track is identified by a Full Track Name, consisting of a Track Namespace and a Track Name.

  したがって §8.7 の 4,096 バイト上限が Redirect target に適用される
- `Redirect::encode_to` と `Redirect::decode_from` の両方で `validate_full_track_name` を呼び、他のメッセージと同じ扱いに揃える。encode 側で拒否することで、上限超過のワイヤフォーマット生成を防ぐ
- namespace-scoped なリクエスト (SUBSCRIBE_NAMESPACE / PUBLISH_NAMESPACE / SUBSCRIBE_TRACKS) の Redirect は Track Name が空でなければならない MUST がある (§9.4.1)。Track Name が空の場合の合計長は namespace 長と等しく、namespace 単体の検査で既に 4,096 バイト以下が保証されるため、この MUST の範囲では新たな拒否は生じない
- §9.4.1 の Connect URI 長の検証 (自 role と元リクエスト種別に依存する) は現状どおり Session 層 (`handle_peer_request_error`) に置いたままとする。本 issue は合計長の検証だけを扱う

## 完了条件

- 合計 4,096 バイトを超える Redirect を持つ REQUEST_ERROR の encode が PROTOCOL_VIOLATION で拒否されることを固定するテストが `tests/test_message.rs` に追加されていること
- 合計 4,096 バイトを超える Redirect を含むバイト列の decode が PROTOCOL_VIOLATION で拒否されることを固定するテストが追加されていること
- 合計がちょうど 4,096 バイトの Redirect が encode / decode とも成功することを固定するテストが追加されていること (境界の非退行)
- namespace 単体が 4,096 バイトを超える既存の拒否挙動が変わらないこと

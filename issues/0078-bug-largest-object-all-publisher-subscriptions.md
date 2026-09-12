# SUBSCRIBE_OK / REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY の LARGEST_OBJECT を全 publisher 役購読の最大から算出する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-largest-object-all-publisher-subscriptions
- Polished: {YYYY-MM-DD}

## 目的

同一 Track に複数の publisher 役 subscription が存在する場合でも、LARGEST_OBJECT を Track 単位の値として広告する。draft-ietf-moq-transport-21 §9.20.18 (LARGEST OBJECT Parameter) は LARGEST_OBJECT を「sending endpoint が観測した Track の largest Location」と定義し、Track に Object が publish 済みなら Publisher は必ず含めなければならない (MUST)。0016 で FETCH
と SUBSCRIBE 処理が `publisher_track_largest` を使うようになり、TRACK_STATUS_OK (`send_ok_for_track_status`) も同ヘルパを使うが、応答系の 3 経路に単一 subscription 由来の算出が残っている。

## 現状

`src/session/subscription/fill.rs` の `publisher_track_largest` は、`subscriptions_by_track` から同一 Track の publisher 役 subscription を引き、`effective_largest_object` の最大値を返す。一方で次の 3 経路は subscription 自身の `effective_largest_object` だけを使う。

- `src/session/subscription/send.rs` の `send_subscribe_ok` は `effective_largest_object(subscription)` の値だけを `update_largest_object_in_parameters` に渡す。新規 subscription は観測値を持たないため、同じ Track の別 subscription で Object を publish 済みでも SUBSCRIBE_OK に LARGEST_OBJECT が付かない (§9.20.18 の MUST 違反)。
- `src/session/subscription/dispatch.rs` の `send_ok_for_subscription` は REQUEST_UPDATE_OK で `context_allows_largest_object` が真のとき `effective_largest_object(subscription)` を注入する。自側 publisher 役では同じ Track の他 subscription の観測値が反映されない。
- `src/session/subscription/send.rs` の `send_publish_state_notify` は `effective_largest_object(subscription)` でパラメータを補完する。同じ Track の他 subscription の観測値が反映されない。

逆に `publisher_track_largest` は `handle_peer_fetch` (`src/session/fetch.rs`)、`send_ok_for_track_status` (`src/session/namespace/track_status.rs`)、`handle_peer_subscribe` と `maybe_open_fill_stream` (`src/session/subscription/recv.rs` / `src/session/subscription/fill.rs`) で使われており、経路によって算出元が非対称になっている。

根拠 (draft-ietf-moq-transport-21 §3.1.3 (Largest Object) / §9.20.18 (LARGEST OBJECT Parameter)):

> The Largest Object is the Object with the largest Location (Section 8.2) in the Track from the perspective of the publisher processing the message.

> It contains the largest Location (see Section 8.2) in the Track observed by the sending endpoint (see Section 3.3.1). If Objects have been published on this Track the Publisher MUST include this parameter.

再現手順 (SUBSCRIBE_OK の例):

1. 同一 Track への 2 本目の SUBSCRIBE を `handle_peer_subscribe` が受理する
2. 1 本目の subscription で Object を publish 済みの状態で `send_subscribe_ok` を呼ぶ
3. 2 本目の subscription は観測値を持たないため、SUBSCRIBE_OK に LARGEST_OBJECT が付かない

## 設計方針

- 自側 publisher 役が送る LARGEST_OBJECT は `publisher_track_largest` で算出する。対象は `send_subscribe_ok`、`send_publish_state_notify`、`send_ok_for_subscription` の REQUEST_UPDATE_OK 注入 (自側 publisher 役のとき) の 3 経路。
- `send_ok_for_subscription` は PUBLISH 起点で自側 subscriber 役の場合も共有する。この場合は `publisher_track_largest` の対象外 (publisher 役のみを集約する) のため、従来どおり `effective_largest_object(subscription)` を使う。subscriber 役での購読横断の集約は本 issue の対象外とする。
- `update_largest_object_in_parameters` の「アプリ指定値との max を取る」挙動は変えず、算出元だけを置き換える。`publisher_track_largest` が `None` のときは LARGEST_OBJECT を付与しない。
- 3 経路それぞれに、同一 Track の別 publisher 役 subscription で観測した値が反映される回帰テストを追加する。

## 完了条件

- 同一 Track の別 publisher 役 subscription の観測値が SUBSCRIBE_OK / REQUEST_UPDATE_OK / PUBLISH_STATE_NOTIFY の LARGEST_OBJECT に反映されること
- 観測値が 1 件も無い場合は LARGEST_OBJECT が付与されないこと
- `send_ok_for_subscription` の subscriber 役 (PUBLISH 起点) 経路の挙動が変わらないこと
- 回帰テストが `tests/test_session/` に追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること

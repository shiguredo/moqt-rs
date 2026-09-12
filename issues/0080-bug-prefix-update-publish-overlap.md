# prefix 更新を跨いだ PUBLISH 紐付けと overlap 検査の残課題を解消する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-prefix-update-publish-overlap
- Polished: {YYYY-MM-DD}

## 目的

SUBSCRIBE_TRACKS / SUBSCRIBE_NAMESPACE の TRACK_NAMESPACE_PREFIX 更新 (draft-ietf-moq-transport-21 §9.5.2 / §9.20.21) について、0017 で確定待ちを導入した後に残っている 2 つの不整合を解消する。

- REQUEST_OK の適用後に、peer が更新を処理する前に送った旧 prefix 基準の PUBLISH が届いても TrackSubscription に紐付くようにする
- prefix 更新の overlap 検査を、同種の active な subscription すべて (役割を問わない) に対して行う

## 現状

### 旧 prefix 基準の PUBLISH の紐付け

`src/session/core.rs` の `recv_request` の PUBLISH 分岐は、自側 subscriber 役の `TrackSubscription` を走査し、確定待ちの prefix (`pending_prefix_updates`) または適用済みの `TrackSubscription::prefix` に一致する PUBLISH の `track_alias` を `active_track_aliases` に登録する。`handle_ok_for_track_subscription`
(`src/session/namespace/track_subscription.rs`) が REQUEST_OK で確定待ちを適用すると、一致対象は新しい prefix だけになる。

PUBLISH は REQUEST_UPDATE とは別の bidi stream で送られるため順序保証がない。peer が REQUEST_UPDATE を処理する前に送った旧 prefix 基準の PUBLISH は REQUEST_OK の適用後に到着しうる (draft §9.5.2: "Updating the prefix of a SUBSCRIBE_TRACKS has no effect on existing subscriptions.")。この PUBLISH は `active_track_aliases` に入らないため、
SUBSCRIBE_TRACKS の bidi stream 終端時に `close_track_subscription_on_stream_end` が行う暗黙終端の対象から漏れる。

確認済みのテスト範囲:

- `tests/test_session/namespace/track_subscription.rs` の `subscribe_tracks_update_prefix_pending_publish_registers_alias` は確定待ちの間に新 prefix の PUBLISH が届くケース
- 同ファイルの `subscribe_tracks_update_prefix_pending_publish_with_old_prefix_registers_alias` は確定待ちの間に旧 prefix の PUBLISH が届くケース
- REQUEST_OK 適用後に旧 prefix の PUBLISH が届くケースのテストはない

### overlap 検査の役割スコープ

prefix 更新の overlap 検査は、検査対象の subscription を役割で絞っている。

- 送信側 (`src/session/namespace/subscribe_namespace.rs` の `send_update_for_namespace_subscription` / `src/session/namespace/track_subscription.rs` の `send_update_for_track_subscription`) は `existing.my_role != TrackRole::Subscriber` の購読を対象外にする
- 受信側 (`handle_update_for_namespace_subscription` / `handle_update_for_track_subscription`) は `existing.my_role == TrackRole::Publisher` の購読だけを対象にする

draft-ietf-moq-transport-21 §9.5.2 は "the new prefix MUST NOT share a common prefix with any other active SUBSCRIBE_NAMESPACE (for a SUBSCRIBE_NAMESPACE update) or SUBSCRIBE_TRACKS (for a SUBSCRIBE_TRACKS update) in the same session"、§9.20.21 は "If the new prefix would share a common prefix
with another active subscription of the same type in the same session, the receiver MUST respond with REQUEST_ERROR with error code PREFIX_OVERLAP" と規定しており、役割の限定はない。

再現手順 (受信側):

1. 自側 subscriber 役の SUBSCRIBE_NAMESPACE を prefix `["a"]` で確立する
2. peer から prefix `["x"]` の SUBSCRIBE_NAMESPACE を受信して確立する
3. peer が prefix を `["a", "b"]` へ変更する REQUEST_UPDATE を送る
4. 受信側の overlap 検査は `my_role == Publisher` の購読だけを見るため、自側 subscriber 役の `["a"]` との overlap を検出せず、prefix が更新される (PREFIX_OVERLAP の REQUEST_ERROR が返らない)

送信側も同様に、自側 subscriber 役の購読から他購読 (publisher 役) と overlap する prefix へ更新する REQUEST_UPDATE を送信できてしまう。

なお初回作成経路 (`send_subscribe_namespace` / `handle_peer_subscribe_namespace` など) にも同じ役割スコープがあるが、本 issue の対象は prefix 更新に限る。上の再現手順 2 の購読が作成できるのはこの作成経路の役割スコープによる。

## 設計方針

- PUBLISH の紐付け: `handle_ok_for_track_subscription` が確定待ちを適用した後も、直前まで有効だった prefix 基準の PUBLISH を `active_track_aliases` に登録できるようにする。確定待ちが複数ある場合 (連続更新) の扱いを含め、どの時点の prefix まで一致対象に残すかは実装時に決め、doc コメントに明記する。0017 と同じく `is_prefix_of` による片方向一致で判定する。
- overlap 検査: 送信側は確定待ちを含む実効 prefix で、受信側は適用済み prefix で、同種の active な subscription すべて (両方の役割、Terminated を除く) と比較する。比較する購読の選択条件は送信側と受信側で揃える。
- 受信側の拒否応答は既存の prefix overlap 拒否と同じ経路 (`emit_request_error` + `REQUEST_PREFIX_OVERLAP`) とし、prefix を反映しない。
- 回帰テストを `tests/test_session/namespace/` に追加する。peer が更新を処理する前に送った旧 prefix の PUBLISH が REQUEST_OK 適用後に届くケース、prefix 更新が役割の異なる購読と overlap するケース (送信側ローカル拒否と受信側 PREFIX_OVERLAP 応答) を含める。

## 完了条件

- REQUEST_OK の適用後に旧 prefix 基準の PUBLISH が届いても `active_track_aliases` に登録されること
- prefix 更新の overlap 検査が役割を問わず同種の active な subscription すべてを対象とすること
- 受信側の overlap 拒否が PREFIX_OVERLAP の REQUEST_ERROR になり、prefix が更新されないこと
- 送信側の overlap 拒否が送信前のローカルエラーになり、確定待ちに残らないこと
- 回帰テストが `tests/test_session/namespace/` に追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること

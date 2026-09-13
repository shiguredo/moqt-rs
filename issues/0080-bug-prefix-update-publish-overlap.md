# prefix 更新を跨いだ PUBLISH 紐付けと overlap 検査の残課題を解消する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-prefix-update-publish-overlap
- Polished: 2026-09-14

## 目的

SUBSCRIBE_TRACKS / SUBSCRIBE_NAMESPACE の TRACK_NAMESPACE_PREFIX (draft-ietf-moq-transport-21 §9.5.2 / §9.15 / §9.18 / §9.20.21) について、0017 で確定待ちを導入した後に残っている 2 つの不整合を解消する。

- REQUEST_OK の適用後に、peer が更新を処理する前に送った旧 prefix 基準の PUBLISH が届いても TrackSubscription に紐付くようにする
- prefix の overlap 検査から役割の限定を外し、同種の購読すべてを対象にして §9.5.2 の MUST NOT (共通 prefix を持つ購読の併存禁止) と §9.15 / §9.18 / §9.20.21 の MUST (PREFIX_OVERLAP の REQUEST_ERROR 応答) を満たす

## 現状

### 旧 prefix 基準の PUBLISH の紐付け

`src/session/core.rs` の `recv_request` の PUBLISH 分岐は、自側 subscriber 役の `TrackSubscription` を走査し (state は見ていない)、確定待ちの prefix (`pending_prefix_updates`) または適用済みの `TrackSubscription::prefix` に一致する PUBLISH の `track_alias` を `active_track_aliases` に登録する。
`handle_ok_for_track_subscription` (`src/session/namespace/track_subscription.rs`) が REQUEST_OK で確定待ちを適用すると `TrackSubscription::prefix` が置換され、確定待ちのキューも空になるため、一致対象は新しい prefix だけになる。

PUBLISH は REQUEST_UPDATE とは別の bidi stream で送られるため順序保証がない。peer が REQUEST_UPDATE を処理する前に送った旧 prefix 基準の PUBLISH は REQUEST_OK の適用後に到着しうる
(draft §9.5.2: "Updating the prefix of a SUBSCRIBE_TRACKS has no effect on existing subscriptions.")。この PUBLISH は `active_track_aliases` に入らないため、
SUBSCRIBE_TRACKS の bidi stream 終端時に `close_track_subscription_on_stream_end` が行う暗黙終端の対象から漏れる。

確認済みのテスト範囲:

- `tests/test_session/namespace/track_subscription.rs` の `subscribe_tracks_update_prefix_pending_publish_registers_alias` は確定待ちの間に新 prefix の PUBLISH が届くケース
- 同ファイルの `subscribe_tracks_update_prefix_pending_publish_with_old_prefix_registers_alias` は確定待ちの間に旧 prefix の PUBLISH が届くケース
- REQUEST_OK 適用後に旧 prefix の PUBLISH が届くケースと、連続更新の中間 prefix で届くケースのテストはない

### overlap 検査の役割スコープ

同種 (SUBSCRIBE_NAMESPACE どうし / SUBSCRIBE_TRACKS どうし) の overlap 検査は、比較対象の購読を役割と状態で絞っている。テーブルは別 (`namespaces` と `track_subscriptions`) で、§9.15 / §9.18 は両者の overlap 空間が独立であると規定する。

- 送信側 (`src/session/namespace/subscribe_namespace.rs` の `send_subscribe_namespace` / `send_update_for_namespace_subscription`、
  `src/session/namespace/track_subscription.rs` の `send_subscribe_tracks` / `send_update_for_track_subscription`) は `existing.my_role != TrackRole::Subscriber` の購読を対象外にする (state は Terminated のみ除外する)
- 受信側 (`handle_peer_subscribe_namespace` / `handle_update_for_namespace_subscription` / `handle_peer_subscribe_tracks` / `handle_update_for_track_subscription`) は `existing.my_role == TrackRole::Publisher` かつ `state == Established` の購読だけを対象にする

draft-ietf-moq-transport-21 の該当規定に購読の役割による限定はない。

- §9.5.2: "the new prefix MUST NOT share a common prefix with any other active SUBSCRIBE_NAMESPACE (for a SUBSCRIBE_NAMESPACE update) or SUBSCRIBE_TRACKS (for a SUBSCRIBE_TRACKS update) in the same session"
- §9.15 / §9.18: "Within a session, if a publisher receives a SUBSCRIBE_NAMESPACE with a Track Namespace Prefix that shares a common prefix with an established SUBSCRIBE_NAMESPACE, it MUST respond with REQUEST_ERROR with error code PREFIX_OVERLAP." (SUBSCRIBE_TRACKS 側も同じ文面)
- §9.20.21: "If the new prefix would share a common prefix with another active subscription of the same type in the same session, the receiver MUST respond with REQUEST_ERROR with error code PREFIX_OVERLAP"

再現手順 (受信側):

1. 自側 subscriber 役の SUBSCRIBE_NAMESPACE を prefix `["a"]` で確立する
2. peer から prefix `["x"]` の SUBSCRIBE_NAMESPACE を受信して確立する (`["x"]` は `["a"]` と overlap しないため作成経路でも受理される)
3. peer が prefix を `["a", "b"]` へ変更する REQUEST_UPDATE を送る
4. 受信側の overlap 検査は `my_role == Publisher` かつ `state == Established` の購読だけを見るため、自側 subscriber 役の `["a"]` との overlap を検出せず、prefix が更新される (PREFIX_OVERLAP の REQUEST_ERROR が返らない)

送信側も同様に、自側 subscriber 役の購読から他購読 (publisher 役) と overlap する prefix へ更新する REQUEST_UPDATE を送信できてしまう。初回作成経路 (手順 2 の経路) も同じ役割スコープを持ち、自側 subscriber 役の購読と overlap する購読を受理してしまう。

## 設計方針

- PUBLISH の紐付け: 確定待ちの適用後も、直前まで有効だった prefix 基準の PUBLISH を紐付けられるようにする。一致対象は「適用済みの `TrackSubscription::prefix`」「確定待ちの全要素」「REQUEST_OK の適用で置換される前の prefix の履歴」の和集合とし、0017 と同じく `is_prefix_of` による片方向一致で判定する。
  - prefix の履歴は、`handle_ok_for_track_subscription` が確定待ちを適用するときに置換前の値を追加して累積させる。連続更新 (REQUEST_UPDATE を複数送り、途中の REQUEST_OK が適用された後に中間 prefix 基準の PUBLISH が届くケース) でも取りこぼさない。履歴は `pending_prefix_updates` と同様に Session 内部に保持する (公開型 `TrackSubscription` のフィールドは増やさない)。
  - 履歴は PUBLISH の照合にのみ使い、overlap 検査の比較対象には使わない。過去の prefix を比較に混ぜると、更新前の prefix が将来の購読を恒久的にブロックし、Terminated の購読だけを除外する既存の前提と衝突する。
  - 履歴は当該 TrackSubscription が Terminated へ遷移したとき (bidi 終端・REQUEST_ERROR など) に破棄する。
  - 照合対象は `my_role == Subscriber` かつ `state != Terminated` の TrackSubscription に限る (Terminated の購読へ alias を登録しない)。
- overlap 検査: 役割条件を外して同種の購読すべてを対象にし、state 条件は送信側と受信側で次に定める。
  - 送信側 (`send_subscribe_namespace` / `send_update_for_namespace_subscription` / `send_subscribe_tracks` / `send_update_for_track_subscription`) は `state != Terminated` の購読を、確定待ちを含む実効 prefix で比較する (0017 が定めた条件を維持する)。
  - 受信側 (`handle_peer_subscribe_namespace` / `handle_update_for_namespace_subscription` / `handle_peer_subscribe_tracks` / `handle_update_for_track_subscription`) は
    `state == Established` の購読を、適用済み prefix で比較する。`Established` に限るのは、自側がまだ確定していない購読
    (自側 publisher 役で REQUEST_OK を返していない購読と、自側 subscriber 役で REQUEST_OK を受け取っていない購読) を根拠に
    peer の購読や更新を拒否しないためである (§9.15 / §9.18 の作成時拒否も "established" を条件にする)。
  - 受信側が適用済み prefix を使うのは、peer が現に認識している値で判定するためである。自側の確定待ちは peer がまだ受理していない (draft §9.5.2: 更新は REQUEST_OK の後で有効になる) ため、比較に混ぜない。
  - 初回作成経路と更新経路で条件を揃える。0017 が「作成時と同じ条件で送信前にローカル検査する」を設計判断として明記しており、更新経路だけを広げると同じ overlap 状態が経路によって可否の異なる仕様になる。
- 受信側の拒否応答は既存の prefix overlap 拒否と同じ経路 (`emit_request_error` + `REQUEST_PREFIX_OVERLAP`) とし、prefix を反映せず、state も変更しない (`emit_request_error` は fin 付きの REQUEST_ERROR を送る)。REQUEST_UPDATE 拒否時の終端を Terminated に揃えるかは 0074 が扱う範囲であり、本 issue では変えない。
- 後方互換: 公開 API のシグネチャは変更しない。overlap する購読の作成・prefix 更新がローカルエラーまたは PREFIX_OVERLAP の REQUEST_ERROR として拒否されるようになり受理条件が狭まるため、`CHANGES.md` には `[FIX]` として記載する。
- 回帰テストを `tests/test_session/namespace/` に追加する。REQUEST_OK 適用後に旧 prefix の PUBLISH が届くケース、連続更新の中間 prefix で届くケース、Terminated の購読が alias 登録の対象外であること、役割の異なる購読と overlap するケース (作成経路と更新経路のそれぞれについて、送信側ローカル拒否と受信側 PREFIX_OVERLAP 応答) を含める。

## 完了条件

- REQUEST_OK の適用後に旧 prefix 基準の PUBLISH が届いても `active_track_aliases` に登録されること
- 連続する prefix 更新の途中で適用された中間 prefix 基準の PUBLISH も `active_track_aliases` に登録されること
- Terminated の TrackSubscription には alias が登録されないこと
- overlap 検査が役割を問わず同種の購読を対象とすること (送信側は `state != Terminated`、受信側は `state == Established`)
- 受信側の overlap 拒否が PREFIX_OVERLAP の REQUEST_ERROR になり、prefix が更新されないこと
- 送信側の overlap 拒否が送信前のローカルエラーになり、確定待ちに残らないこと
- 初回作成経路と prefix 更新経路で overlap 検査の条件が同じであること (送信側ローカル拒否と受信側 PREFIX_OVERLAP 応答の両方)
- 回帰テストが `tests/test_session/namespace/` に追加され、`cargo test --workspace` が通ること
- `CHANGES.md` の `## develop` に `[FIX]` エントリが追加されていること

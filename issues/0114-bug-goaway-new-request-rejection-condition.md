# GOAWAY 後の新規 request 拒否条件を仕様の主語に合わせる

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-goaway-new-request-rejection-condition
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §9.2 (GOAWAY) が新規 request の拒否を MAY として認める箇所は 2 つあり、主語がそれぞれ異なる。

> The GOAWAY message does not impact subscription state.  A subscriber
> SHOULD individually unsubscribe from each existing subscription,
> while a publisher MAY reject new requests after sending a GOAWAY.

> An endpoint that receives a GOAWAY MAY reject new requests with an
> appropriate error code (e.g., REQUEST_ERROR with error code
> GOING_AWAY).

現状の実装は 1 文目だけを、publisher という限定を外して適用している。自側が GOAWAY を送ったかどうか (`local_sent`) のみを条件にするため、publisher としてふるまっていない endpoint や、publisher が受ける request ではない種別まで拒否しうる。2 文目の「GOAWAY を受信した endpoint が拒否しうる」経路は実装していない。拒否条件を仕様の主語に合わせ、どちらの MAY を採るかを実装として確定させる。

## 現状

- `src/session/core.rs` の `Session::accept_peer_request` が `self.goaway.local_sent` を条件に `REQUEST_GOING_AWAY` の `REQUEST_ERROR` を発行して `Ok(false)` を返す。doc コメントも「control GOAWAY 送信済み (`local_sent`) なら」と送信側基準で書いており、role と request 種別を見ない。
- peer から GOAWAY を受信したことは `src/session/goaway.rs` の `Session::handle_peer_goaway` が `goaway.peer` に記録し、`Session::check_peer_goaway` が自側からの新規 request 送信を `SendRequestError::PeerGoawayReceived` で抑制する。受信済み GOAWAY を理由に peer からの新規 request を拒否する経路は無い。
- `tests/test_session/goaway.rs` の `local_goaway_rejects_peer_new_subscribe` が送信側基準の拒否を固定している。同ファイルの `peer_goaway_does_not_trigger_local_going_away_reject` は「peer の GOAWAY を受信しただけでは自側は拒否しない」ことを回帰テストとして固定しており、2 文目の MAY を現状は使わないという判断が既に入っている。
- `Session::validate_peer_request_id` の doc のとおり、既存 subscription への REQUEST_UPDATE は GOAWAY の影響を受けず、GOING_AWAY 拒否の対象外である。

## 設計方針

- 1 文目の適用範囲を仕様の主語に合わせる。publisher として受ける request に限る等の条件を立て、role と request 種別で判定する。条件を狭める場合は `tests/test_session/goaway.rs` の送信側基準のテスト (`local_goaway_rejects_peer_new_subscribe` ほか) の期待値を実装に合わせて更新する。
- 2 文目の MAY を採るかどうかを決める。採る場合は「peer から GOAWAY を受信済み」を拒否条件に加え、`peer_goaway_does_not_trigger_local_going_away_reject` の期待値を新しい判断に合わせて書き換える。採らない場合は、MAY を使わず受理するという判断であることを doc コメントに残し、GOAWAY 受信後の新規 request を受理することをテストで固定する。
- 拒否は `REQUEST_ERROR` + 自側送信方向の FIN とし、セッションは維持する (draft §6.4.2.3)。既存 subscription への REQUEST_UPDATE は拒否しない (§9.2 "The GOAWAY message does not impact subscription state.")。
- GOAWAY timeout と drain blocker の評価順・対象が変わらないことを確認する。

## 完了条件

- GOAWAY を受信した後の新規 request の扱いを固定するテストが追加されていること。
- 自側 GOAWAY 送信後にどの role / request 種別を拒否するかが決まり、拒否と受理の両方がテストで固定されていること。
- `tests/test_session/goaway.rs` の GOING_AWAY 関連テストが実装と矛盾しないこと。
- `cargo test --workspace` が通ること。

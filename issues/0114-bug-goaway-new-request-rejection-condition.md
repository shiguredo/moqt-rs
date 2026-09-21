# GOAWAY 後の新規 request 拒否条件を仕様の主語に合わせる

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-goaway-new-request-rejection-condition
- Polished: 2026-09-21

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

- 1 文目の MAY を採る。ただし仕様のとおり publisher に限定する。自側が control GOAWAY を送信済みで、かつ受信した request に対して自側が publisher として応答する場合だけ拒否する
- 判定は request 種別で行う。自側が publisher として応答するのは SUBSCRIBE (`Session::handle_peer_subscribe`) / FETCH (`Session::handle_peer_fetch`) / TRACK_STATUS (0109 で追加される受信ハンドラ) である。PUBLISH (`Session::handle_peer_publish`) は自側が subscriber として受けるため拒否しない
- `Session` の `Role` (Client / Server) はエンドポイントの役割であり publisher / subscriber ではない。publisher かどうかは既存 subscription の `TrackRole` ではなく受信した request の種別で決まるため、`Session::accept_peer_request` に request 種別を渡して判定する
- 2 文目の MAY は採らない。§9.2 は「GOAWAY を送った側も migration のために新規 request を開始できる (SHOULD avoid ただし required by migration を除く)」と定めており、受信側が一律に拒否すると migration を阻害する。採らない判断であることを `Session::accept_peer_request` の doc コメントに残し、GOAWAY 受信後も peer の新規 request を受理する挙動を維持する
- 1 文目はエラーコードを指定していない。§12.3 (Request Error Codes) は `GOING_AWAY` を「GOAWAY を受信した endpoint」の語として定義しており、送信側の拒否に使うと registry の定義とはずれる。本実装は peer に「この endpoint を離れる」ことを伝える最も近い定義済みコードとして `GOING_AWAY` を使い、この選択理由を doc コメントに明記する
- 拒否は `REQUEST_ERROR(GOING_AWAY)` + 自側送信方向の FIN とし、セッションは維持する。§6.4.2.3 は "When an endpoint rejects a request without performing any application processing, it SHOULD send a REQUEST_ERROR and FIN the stream." と定める。セッションを閉じないのは `Session::fail` を呼ばず `Session::emit_request_error` だけを使うためであり、§6.4.2.3 がセッション維持を定めているわけではない
- request stream 上の GOAWAY (`Session::send_goaway_on_request_stream`) は `local_sent` を立てないため、新規 request の拒否には影響しない。この不変条件を維持する
- 既存 subscription への REQUEST_UPDATE は拒否しない (§9.2 "The GOAWAY message does not impact subscription state.")
- 受信 PUBLISH を受理するようになるため、Pending の subscription が GOAWAY drain blocker に加わりうる。drain blocker の評価順と `Session::tick` の GOAWAY_TIMEOUT 判定は変えず、この追加を許容する
- `tests/test_session/goaway.rs` の GOING_AWAY 経路のテスト 8 件は期待値を変えない (拒否そのものを固定するものと、拒否後の id 検証・auth token 適用を固定するものを含む)
- 対象は `local_goaway_rejects_peer_new_subscribe` / `goaway_rejected_request_stream_close_does_not_fail_session` / `goaway_rejected_request_double_close_fails_as_unknown_id` / `goaway_rejected_request_id_resend_closes_with_invalid_request_id` / `goaway_local_sent_parity_invalid_closes_with_invalid_request_id`
- `goaway_sent_side_registers_auth_token_from_going_away_rejected_subscribe` / `goaway_sent_side_register_overflow_takes_priority_over_going_away` / `goaway_sent_side_use_alias_after_goaway_does_not_kill_session` も期待値を変えない
- `peer_goaway_does_not_trigger_local_going_away_reject` と `goaway_on_request_stream_does_not_reject_new_peer_requests` は現行の挙動を固定する回帰テストとして維持する

## 完了条件

- 自側が control GOAWAY を送信済みのとき、受信した SUBSCRIBE / FETCH / TRACK_STATUS を `REQUEST_ERROR(GOING_AWAY)` + FIN で拒否し、セッションが `SessionState::Established` のままであることがテストで固定されていること
- 自側が control GOAWAY を送信済みでも、受信した PUBLISH を拒否せず受理することがテストで固定されていること
- control GOAWAY を送信していないとき、および request stream 上の GOAWAY だけを送ったときは、peer の新規 request を拒否しないことがテストで固定されていること
- GOAWAY を受信した後の peer の新規 request を受理することがテストで固定されていること (2 文目の MAY を採らない判断)
- 既存 subscription への REQUEST_UPDATE が引き続き拒否されないことがテストで固定されていること
- `tests/test_session/goaway.rs` の GOING_AWAY 関連テストが実装と矛盾しないこと
- `cargo test --workspace` が通ること

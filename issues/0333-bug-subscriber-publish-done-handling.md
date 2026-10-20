# moq-sub が PUBLISH_DONE を status code と購読単位で扱う

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscriber-publish-done-handling
- Polished: 2026-10-20

## 目的

draft-ietf-moq-transport-22 §9.9 (PUBLISH_DONE) と §12.4 (Publish Done Codes) は、PUBLISH_DONE がその subscription の終端であり、status code ごとに意味が異なることを定める。moq-sub は PUBLISH_DONE を 1 通受けると status code も track も問わずパイプライン全体を停止するため、ある track の購読だけが終了しても他の track の受信・再生が止まる。購読単位の終了と status code 別の扱いを実装する。

## 現状

- `examples/moq-sub/src/pipeline.rs` のメインループは `SessionEvent::PublishDoneReceived` を受けると `Ok(true)` を返し、`peer_ended` としてパイプライン全体を終了させる。`request_id` は `..` で捨てており、どの購読の PUBLISH_DONE かを見ていない。
- `status_code` / `stream_count` はログに出すだけで、example 側の終端判定や購読ごとの終了に使っていない。
- `Session` は §9.9 の drain timer と Stream Count の突き合わせを実装済みである。
  - `src/session/types.rs` の `SubscriptionPublishDone` が `drain` / `stream_count` / `stream_count_overrun` を保持し、`Subscription::cleanup_ready` が「drain timer 満了または stream_count 超過」かつ「open な受信 stream が 0」で true になる。
  - `src/session/subscription/delivery.rs` の `record_publish_done` が effective delivery timeout の大きい方を drain timer にする。
  - `examples/tokio-moq/src/moqt_client.rs` の `forget_ready_requests` が `cleanup_ready` と `pending_publish_done` を見て subscription を回収する。
  - `tests/test_session/subscription/publish_done.rs` の `publish_done_drain_waits_for_timeout_and_open_stream_close` と `publish_done_overrun_allows_early_cleanup_after_open_streams_close` がこの挙動を固定している。
- example は `Session` の cleanup 完了を待たずにパイプライン全体を終了する。PUBLISH_DONE 受信時点で `Session` の subscription は Terminated に遷移済みだが、drain 中の late-arriving stream は `Session` が受理する。
- 影響: 1 track の終了が全体停止になる。たとえば publisher が audio subscription だけを `TRACK_ENDED` (0x2) で終了させても、video の受信・表示が止まる。catalog の PUBLISH_DONE でも同じことが起こる。
- MSF §11.3 はライブ配信の終了時に全 active track へ Track Ended を送ると定めるため、audio だけが終了するケースは MSF §11.3 の手順ではない。track 単位の終了は §9.9 / §12.4 の `TRACK_ENDED` やエラー status code で起こりうる。
- 現行の moq-pub は終了時に `PUBLISH_DONE(GOING_AWAY)` を全 track へ送るだけで、track 単位の `TRACK_ENDED` を送らない。したがって audio だけの終了は relay やテスト用 publisher で再現する。example のコード経路は request_id / status_code を見ないため、どの publisher でも同じ全体停止になる。
- §9.9 は subscriber に「状態破棄時に subscription に関連する全 stream へ STOP_SENDING を送る」SHOULD を課す。`Session::forget_subscription` の STOP_SENDING 指示は 0318 の範囲であり、本 issue では example が PUBLISH_DONE で Terminated になった購読へ `stop_sending` を送らないことだけを扱う。0318 の実装後は `MoqtClient` の回収経路が STOP_SENDING を指示するため、example はその指示に従う。

## 再現手順

1. moq-sub を relay に接続し、catalog / video / audio を購読する。
2. publisher または relay が audio subscription の bidi request stream にだけ `PUBLISH_DONE(status_code=TRACK_ENDED, stream_count=...)` を送る。
3. 現状は audio の PUBLISH_DONE で `run` のメインループが `peer_ended` になり、video の受信・デコード・表示も止まる。
4. 期待: audio だけが終了し、video と catalog の購読は継続する。

現行の moq-pub は track 単位の `TRACK_ENDED` を送らないため、再現には relay または `peer_harness` を使ったテスト publisher が必要である。pipeline の統合テストは 0338 で追加する。

## 設計方針

- `SessionEvent::PublishDoneReceived { request_id, status_code, stream_count, reason }` の `request_id` を、現在の `catalog_request_id` / `video_request_id` / `audio_request_id` と照合して `TrackKind` を決める。移行で request_id は変わるため、現在値だけを見る。対応しない request_id (移行前の旧 id など) は debug ログを出して無視する。
- drain timer と Stream Count の突き合わせは `Session` の既存実装に任せ、example で再実装しない。
  - example は PUBLISH_DONE を受けた購読を「終了待ち」として `HashMap<u64, (TrackKind, PublishDoneDisposition)>` に記録し、`MoqtClient` に追加する `subscription_cleanup_ready(request_id) -> Option<bool>` を 100 ms tick で確認する。
  - `Some(true)` または `None` (`Session` が回収済み) になった時点で該当 track を終了する。`Some(false)` の間は late-arriving object を既存の受信経路で処理する。
  - `Session::subscription_cleanup_ready` は `cleanup_ready` だけを見て `pending_publish_done` を見ないが、subscriber 側では `pending_publish_done` は `None` なので問題ない。
  - この accessor は `examples/tokio-moq/src/moqt_client.rs` に doc コメント付きで追加し、未知 request id で `None` を返す単体テストを追加する。
- status code 別の扱い:
  - `TRACK_ENDED` (0x2): 正常終了。該当 track を恒久的に終了する。移行先では購読しない。
  - `GOING_AWAY` (0x4): 該当 track を終了待ちにし、cleanup 後に「移行待ち」として保持する。セッション移行の判断は既存の `SessionEvent::GoawayReceived` 経路が担う。PUBLISH_DONE(GOING_AWAY) を移行のトリガーにしない。移行が成立した場合は移行先で購読を張り直して track を再開する。全 media track が恒久的に終了し、移行待ちも無い場合は移行せず終了してよい。
  - その他の status code はエラーとして warn ログを出し、該当 track を恒久的に終了する。移行先では購読しない。セッション全体は閉じない。自動再購読はしない。
    - 対象: `INTERNAL_ERROR` 0x0 / `UNAUTHORIZED` 0x1 / `TOO_FAR_BEHIND` 0x5 / `EXPIRED` 0x6 / `UPDATE_FAILED` 0x8 / `EXCESSIVE_LOAD` 0x9 / `MALFORMED_TRACK` 0x12、および未知の code。
    - `UPDATE_FAILED` は example が video に送る `REQUEST_UPDATE` の失敗応答であり、その旨をログに残す。
- track の終了処理:
  - 終了待ちの subscription を `HashMap<u64, (TrackKind, PublishDoneDisposition)>` で保持し、現在の購読の request_id を `active_requests` に保持する。`PublishDoneDisposition` は `TrackEnded` / `GoingAway` / `Error` を表す。
  - 起動時は購読した catalog / video / audio の request_id で `active_requests` を、購読した media track で `active_media_tracks` を初期化する。
  - cleanup 可能になったら、終了待ちの entry を除去し、`track_map` と datagram 受信タスクが持つ alias 表の両方から該当 alias を除去し、更新後の表を `datagram_session_tx` で渡す。以後の新規 stream / datagram は未知 alias として読み捨てる。
  - cleanup 後に `TrackEnded` / `Error` の track は `permanently_ended_tracks` へ、`GoingAway` の track は `migration_pending_tracks` へ移す。`active_media_tracks` からは除去する。
  - 処理中の stream task は FIN / RESET まで読み切る (late-arriving object の受理を妨げない)。表示・録音は既存の経路のまま。
  - catalog も同じ状態機械で扱う。catalog の恒久終了は media を止めない。cleanup 後に catalog の alias を `track_map` から除去し、以後の新規 catalog stream を読み捨てる。処理中の stream が届ける late object は従来どおり `catalog_state` へ適用してよい。最後の catalog 状態は保持する。
  - 全 media track が `permanently_ended_tracks` にあり、media の `migration_pending_tracks` も空なら `peer_ended` を true にしてメインループを抜ける。catalog だけが残っていても、media が無ければ終了する。
- 後始末:
  - PUBLISH_DONE で Terminated になった subscription には `client.stop_sending` を送らない (`Session::stop_sending` は Pending(Subscriber) / Established だけを許可する)。0100 の警告を再発させない。
  - PUBLISH_DONE を受けた時点で `active_requests` から request_id を除き、shutdown 時の STOP_SENDING は「まだ active な購読」だけに送る。`should_stop_sending(peer_ended, session_terminated)` の全体判定だけに頼らない。
- 移行:
  - 移行成功時は `ResubscribePlan` を、catalog が `permanently_ended_tracks` でなければ catalog を含め、`active_media_tracks` と `migration_pending_tracks` に含まれる media track だけで組み立てる。`permanently_ended_tracks` の media track は移行先で購読しない。
  - catalog が `permanently_ended_tracks` にある場合は catalog の購読も張り直さず、最後の `catalog_state` を使う。`resubscribe_tracks` / `Resubscribed` は catalog を `Option` で扱えるようにする。
  - 移行成功後は、張り直した media track を `active_media_tracks` に入れ、`migration_pending_tracks` をクリアする。`permanently_ended_tracks` は維持する。`active_requests` は新しい購読の request_id で初期化する。
  - 移行先で購読を張り直せなかった場合は既存どおり移行失敗として終了する。
- テスト:
  - `publish_done_disposition(status_code) -> PublishDoneDisposition` のような純関数を `examples/moq-sub/src/pipeline.rs` に切り出し、`TRACK_ENDED` / `GOING_AWAY` / その他 / 未知 code の分類を単体テストで固定する。
  - `track_kind_for_request_id` の単体テストで catalog / video / audio / 未知 id の対応を固定する。
  - shutdown 時の STOP_SENDING 対象の判定 (`active_requests` に含まれる request id だけを送る) を純関数または単体テストで固定する。
  - pipeline の統合経路 (audio だけ終了しても video が止まらないこと、全 media 終了で終了すること) は 0338 の統合テストで扱う。本 issue では 0338 のテスト基盤を前提にしない。

## 完了条件

- audio の `PUBLISH_DONE(TRACK_ENDED)` で video と catalog の購読が継続すること。
- catalog の `PUBLISH_DONE` で video / audio が止まらないこと。
- PUBLISH_DONE を受けた購読は `Session` の `cleanup_ready` が true になるまで終了待ちとし、drain 中の late object を処理したうえで該当 track を終了すること。
- `MoqtClient::subscription_cleanup_ready` が追加され、未知 request id で `None` を返すこと。
- 全 media track が恒久的に終了し、media の移行待ちも無い場合にメインループが終了し、録画の finalize などの後始末が走ること。
- 移行時に `permanently_ended_tracks` の media track と catalog を購読し直さず、`migration_pending_tracks` の track だけを再開すること。
- status code の分類 (`TRACK_ENDED` / `GOING_AWAY` / その他 / 未知) が純関数の単体テストで固定されていること。
- request_id から track 種別への対応が単体テストで固定されていること。
- PUBLISH_DONE で Terminated になった購読へ STOP_SENDING を送らず、shutdown 時に警告が出ないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

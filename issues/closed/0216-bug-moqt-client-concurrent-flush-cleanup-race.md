# 別タスクからの flush と request 回収が競合して PUBLISH_DONE を失う

- Created: 2026-10-09
- Completed: 2026-10-13
- Branch: feature/fix-moqt-client-concurrent-flush-cleanup-race
- Polished: 2026-10-13
- Updated: 2026-10-10

## 目的

0201 で request の回収条件に保留中の PUBLISH_DONE を加えたが、保証は「Session のイベントキューが
空になってから回収する」ところまでである。`send_data_stream_closed` を別タスクから呼ぶ配線では、
キューが空であることを観測してから回収がロックを取るまでの間に flush が `SendOnStream` を積む競合が
残る。回収が先に走ると subscription が破棄され、積まれた PUBLISH_DONE は送信半が無いため捨てられる。
これは仕様の 2 つの要求を同時に破る。

- draft-ietf-moq-transport-22 §9.5.1 (Updating Subscriptions): REQUEST_UPDATE を拒否した publisher は
  `PUBLISH_DONE` (`UPDATE_FAILED`) を送る MUST がある。
- 同 §9.9 (PUBLISH_DONE): "A sender MUST NOT destroy subscription state until it sends PUBLISH_DONE,
  though it can choose to stop sending objects (and thus send PUBLISH_DONE) for any reason."
  (subscription の状態は PUBLISH_DONE を送るまで破棄してはならない)

配線を増やしたときに黙って壊れない状態にする。

## 現状

- `examples/tokio-moq/src/moqt_client.rs` の `MoqtClient::drain_events` は最後の `Session::poll_event` が
  `None` を返すとロックを解放し、その後に `MoqtClient::cleanup_closed_requests` が同じロックを
  取り直して `should_forget_subscription` (`cleanup_ready` が真かつ `Subscription::pending_publish_done`
  が `None`) が真なら `Session::forget_subscription` を呼ぶ。`cleanup_closed_requests` は
  `drain_events` の末尾と `CloseSession` の早期 return 前から呼ばれる。
- ロックを解放してから取り直すまでの間に別タスクの flush が入ると、回収は `pending_publish_done` が
  `None` になった直後を観測して subscription を破棄する。その後で `SendOnStream { fin: true }` が
  残っていても送信半が無いため `SendOnStreamStep::Skip` となり PUBLISH_DONE は書き出されない。
- flush の契機は `Session::maybe_flush_pending_publish_done` の呼び出し 4 箇所である。
  `Session::reset_outgoing_data_stream_at_with_code` の 2 経路 (subgroup stream の reset と fill fetch
  stream の reset)、`Session::send_data_stream_closed`、fill fetch stream への STOP_SENDING。このうち別タスクから呼べるのは `DataPlaneHandle::send_data_stream_closed` だけで
  ある (`reset_outgoing_data_stream*` と `recv_data_stream_stop_sending` は `Session` の API で、
  `DataPlaneHandle` には無い)。fill fetch stream への STOP_SENDING は `MoqtClient::pump_once` が
  `Session` へ直接呼び、直後に同じタスクで `drain_events` するため競合しない。
- `pending_publish_done` を `Some` にするのは driver タスクだけが呼べる `Session::send_request_error` と
  `Session::handle_err_for_subscription` であり、別タスクは保留を作れない。
- 競合が成立する前提は 4 つである。別タスクから `send_data_stream_closed` を呼ぶ配線、request が
  回収対象として登録済み、publisher 役で `Terminated` かつ `pending_publish_done` が `Some`、最後の
  outgoing subgroup stream が open のまま。
- 現在の examples は flush の契機を別タスクから呼ぶ配線を持たないため到達しない (コードにも制限として
  明記済み)。moq-pub は `SubgroupWriter` を単一タスクで駆動し、moq-sub が spawn したタスクは受信系 API と
  `send_data_stream_stop_sending` を呼ぶが、後者は incoming stream 用で flush の契機ではない。
- 0201 の解決方法と `cleanup_closed_requests` の doc には「キューが空になってから回収する保証まで」と
  「別タスクから flush を呼ぶ配線では競合が残る」が記録されているが、競合の窓 (ロックの解放と再取得の
  間) と前提 4 条件は書かれていない。
- 0215 (PUBLISH 起点 publisher の保留 PUBLISH_DONE が request stream の FIN で失われる) は、保留が
  publisher 役に限られ `SessionEvent::FinishRequestStream` が subscriber 役に限られるという役割の
  不変条件で到達不能と確定して closed になった。本 issue の経路は example の配線に依存する別経路で
  あり、同じ理由では閉じない。

## 設計方針

- 競合の窓はロックの解放と再取得の間だけであるため、example 側で
  「キューが空であることの観測」と「回収の判定と `Session::forget_subscription`」を同一のロック保持で
  行えば閉じる。別タスクの flush はそのロックで待たされるため、`pending_publish_done` が `None` に
  なった直後を回収が観測してしまう順序は起きない。
  - 回収の判定は引き続き `should_forget_subscription` の純関数に保つ。
  - ロック保持中は同期処理だけで完結させ、送信半の `finish()` は従来どおりロックを解放してから行う。
- Session 側の API 追加 (キューに該当 request のイベントが残っているかを問い合わせる API) と、
  `Session::forget_subscription` の契約変更は行わない。前者は example 側の同一ロック化で不要になり、
  後者は 3 つの既存契約と衝突する。
  - `Session::forget_subscription` は保留中の PUBLISH_DONE を破棄する契約で、テスト
    (`tests/test_session/subscription/request_update.rs` の
    `pending_publish_done_discarded_on_forget_subscription`) が固定している。
  - GOAWAY timeout は request stream を RESET した後に PUBLISH_DONE を書けないため、意図的に保留を
    破棄する (`tests/test_session/goaway.rs` の
    `request_stream_goaway_reset_discards_pending_publish_done`)。
  - `Session::forget_subscription` の doc と `skills/shiguredo-moqt/SKILL.md` は「保留中の
    PUBLISH_DONE がある間は呼ばないこと」を呼び出し側の責務として明記しており、これを反転させると
    契約が二重になる。
- 変更対象は次のとおりである。
  - `examples/tokio-moq/src/moqt_client.rs`: 回収の判定と `forget_subscription` をキューが空である
    観測と同じロック保持で行う。`cleanup_closed_requests` の doc を新しい契約に合わせて更新する。
  - `tests/test_session/subscription/request_update.rs`: 最後の outgoing data stream を閉じたときに
    PUBLISH_DONE の `SendOnStream { fin: true }` が 1 件だけ積まれ、`pending_publish_done` が `None` に
    なり、`poll_event` で取り出せることを公開 API で固定する。RESET 経路は既存テスト
    (`pending_publish_done_flushed_on_data_stream_closed_reset`) が固定済みであるため、FIN 経路の
    検証に `fin: true` のアサートを足して両経路を揃える。
- 「回収は Session のイベントを I/O へ変換し終えた後にだけ行う」契約と、`tick` から回収しない方針は
  維持する。`CloseSession` の早期 return 前の回収はキューが空である観測を伴わないが、セッション終了後は
  flush の契機がすべて失敗するため現状のままにする。

## 完了条件

- 別タスクから `send_data_stream_closed` を呼ぶ配線でも、flush された PUBLISH_DONE が request stream へ
  書き出されること。競合の窓がロックの解放と再取得の間に閉じていることをコードで確認する
  (キューが空である観測と回収の判定が同一のロック保持で行われていること)。
- 上記を支える Session 側の挙動が `tests/test_session/subscription/request_update.rs` で固定されて
  いること (最後の outgoing data stream を閉じると PUBLISH_DONE の `SendOnStream { fin: true }` が
  1 件積まれ、`poll_event` で取り出せること)。RESET 経路は
  `pending_publish_done_flushed_on_data_stream_closed_reset` が `fin: true` まで固定済みであるため、
  FIN 経路の `send_subgroup_object_to_terminated_subscription_rejected` に
  `take_send_on_stream_with_fin` を使った `fin: true` のアサートを足して両経路を揃える。
- 二タスクの割り込みそのものはロックの解放と再取得の間でしか起きず、割り込みのタイミングを公開 API で
  指定できないため決定的に再現できない。実 transport が必要であるため、
  0201・0214・0215 と同じく **I/O ハンドルが必要な配線部分はレビューで確認する**。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること

## 解決方法

- `examples/tokio-moq/src/moqt_client.rs` の `drain_events` で、`Session::poll_event` が `None` を返した
  「キューが空である観測」と、終端済み request の回収の判定・破棄を同一のロック保持で行うようにした。
  別タスクから呼べる flush の契機 (`DataPlaneHandle::send_data_stream_closed`) は同じ
  `Arc<StdMutex<Session>>` を取るため、観測と判定の間に flush が `SessionEvent::SendOnStream`
  (PUBLISH_DONE) を積むことはない。flush が先に起きていればそのイベントがキューに残り `poll_event` が
  `Some` を返すため判定には到達しない (取り出し済みなら送出済みか、`next_event` の future 破棄で
  失われている)。これで回収によって PUBLISH_DONE が失われる経路は閉じた。
- 判定と破棄を `forget_ready_requests` (free 関数) に、後始末を `MoqtClient::finish_forgotten` に
  切り出した。判定と `Session::forget_subscription` / `Session::forget_fetch` は分離できない
  (分離するとロックを解放した隙に flush が入る) ため `forget_ready_requests` が破棄まで行う。
  個々の判定は `should_forget_subscription` の純関数に保っている。送信半の `finish()` はロックを
  解放してから待つ (`finish_forgotten`)。
- `cleanup_closed_requests` は `CloseSession` で早期 return する分岐専用として残した。この経路は
  キューが空である観測を伴わないが、セッション終了後は flush の契機がすべて `require_established` で
  失敗するため新たな `SendOnStream` は積まれない。保留中の subscription は回収しないので、未送出の
  保留はそのまま残る。`tick` から回収しない方針も維持している。
- `tests/test_session/subscription/request_update.rs` の
  `send_subgroup_object_to_terminated_subscription_rejected` に `take_send_on_stream_with_fin` を
  使った `fin: true` のアサートを足し、最後の outgoing data stream の終端で PUBLISH_DONE と同時に
  送信方向が閉じることを RESET 経路と揃えて固定した。
- 二タスクの割り込みそのものはロックの解放と再取得の間でしか起きず、割り込みのタイミングを公開 API で
  指定できないため決定的に再現できない。0201・0214・0215 と同じく配線部分はレビューで確認した
  (同一 Mutex の共有、`require_established` を通る flush の 4 契機、`poll_event` の FIFO 順)。
- 検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` の通過と、`/review-diff-code` 4 周 (致命的 0 件) で行った。

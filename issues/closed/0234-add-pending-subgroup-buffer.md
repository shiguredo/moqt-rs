# Track Alias 未確立の Subgroup ストリームを短時間保持するバッファを追加する

- Created: 2026-10-10
- Completed: 2026-10-10
- Branch: feature/add-pending-subgroup-buffer

## 目的

draft-ietf-moq-transport-22 §3.1.3.1 (Unknown Track Alias) は、Track Alias がまだ Established subscription と結び付いていない Subgroup ストリームを受け取ったとき、データを捨ててもよいし、Track Alias を確立する制御メッセージとの順序の入れ替わりに備えて短時間保持してもよい ("MAY drop the data or buffer it briefly") とする。

Track Alias は SUBSCRIBE_OK などの制御メッセージで確立されるが、制御メッセージとデータストリームは別の経路で届くため、データが先に届くことがある。保持せずに捨てると、映像や音声の先頭の Object が欠ける。欠けた Object は参照フレームの欠落として後続にも影響する。

## 現状

- `src/session/data.rs` の `recv_subgroup_header` は、未知の Track Alias に対して `TrackDataAcceptance::UnknownTrackAlias` を返す。契約上、caller が drop / abandon / short buffer を選ぶ
- `TrackDataAcceptance` は `Discarded` (キャンセル済み subscription の alias。確実に捨ててよい) と `FilteredOut` を `UnknownTrackAlias` と区別しており、「知らない alias なので保持するか判断が必要」な場合だけを caller が切り分けられる
- `examples/moq-sub/src/pipeline.rs` は `UnknownTrackAlias` を破棄する (catalog ではエラーにする)。保持していない
- moqt-js の `src/pendingSubgroupBuffer.ts` の `PendingSubgroupBuffer` が同じ役割を持つが、moqt-rs には移植されていない

## 設計方針

moqt-js の `PendingSubgroupBuffer` を移植し、純粋なロジックとしてライブラリに置く (Sans-I/O。時刻は引数で受ける)。`Promise` で通知する代わりに、caller が poll する形にする。

- `src/pending_subgroup_buffer.rs` に `PendingSubgroupBuffer` を置く。`src/lib.rs` に `pub mod pending_subgroup_buffer;` を追加する
- オプションと既定値
  - `per_stream_max_bytes`: 1 MiB。超えた entry は破棄する
  - `per_session_max_bytes`: 16 MiB。超えた最後の entry を破棄し、残りを巻き込まないようにする
  - `timeout_us`: 5 秒 (§3.1.3.1 の "buffer it briefly" の上限)
- 保持する理由 (通知の理由) は、購読の確立 (`Subscriber`) / 時間切れ (`Timeout`) / stream ごとの上限超過 (`OverflowPerStream`) / session の上限超過 (`OverflowPerSession`) / session の終了 (`SessionClose`) / ストリームの終端 (`EndOfStream`) の 6 つ
- API (poll 形式)
  - `add(track_alias, stream_id, now_us)` で entry を作る
  - `push(stream_id, chunk, now_us)` でチャンクを足す。上限を超えたら破棄する
  - 破棄済みの entry にチャンクを足さない (足すと破棄したはずのバイトが集計に残り、無関係な stream を巻き込んで上限超過させる)
  - `note_subscriber(track_alias)` で購読が確立したことを伝え、その alias の entry を引き取り待ちにする
  - `take_ready(now_us)` で「引き取るべき entry」と理由を取り出す (購読の確立・時間切れ・上限超過・session の終了・ストリームの終端)
  - `remove(stream_id)` で entry を外し、集計からバイトを引く
  - `stream_count()` / `total_bytes()`
  - `reset()`
- バイト数の集計は entry の追加・破棄・取り出し・削除で必ず一致させる (合計が `per_session_max_bytes` を超えないこと)
- `examples/moq-sub/src/pipeline.rs` で、`UnknownTrackAlias` の Subgroup ストリームを保持し、購読が確立したら続きから読む。時間切れ・上限超過・session の終了・ストリームの終端では理由をログに出して破棄する

## 完了条件

- `PendingSubgroupBuffer` がライブラリから使えること
- moq-sub が `UnknownTrackAlias` の Subgroup ストリームを短時間保持し、購読の確立後は続きから読めること
- 上限 (stream ごと / session 全体) と時間切れで破棄され、集計が一致することがテストで固定されていること
- `Discarded` / `FilteredOut` は保持せず捨てること (保持すると不要なデータが上限を圧迫する)
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

- `src/pending_subgroup_buffer.rs` を追加し、moqt-js の `src/pendingSubgroupBuffer.ts` の `PendingSubgroupBuffer` を移植した (時刻は引数で受ける Sans-I/O)
- 公開した: `PendingSubgroupBufferOptions` (per-stream 1 MiB / per-session 16 MiB / timeout 5 秒) と `DEFAULT_PENDING_SUBGROUP_BUFFER_OPTIONS`、`PendingNotifyReason` (`Subscriber` / `Timeout` / `OverflowPerStream` / `OverflowPerSession` / `SessionClose` / `EndOfStream`)、
  `PendingSubgroupEntryId`、`PendingSubgroupReady`、`PendingSubgroupBuffer` (`add` / `push` / `note_subscriber` / `note_end_of_stream` / `note_session_close` / `take_ready` / `take_ready_for` / `remove` / `stream_count` / `total_bytes` / `reset`)
- moqt-js の `Promise` による通知は `take_ready` / `take_ready_for` の poll に置き換えた。引き取るとチャンクの所有権が呼び出し側へ移り、そのぶん集計から外れる。同じ entry は 2 度返らない
- `take_ready_for(id, now_us)` は指定した entry だけを対象にする。複数の stream task が 1 つのバッファを共有しても、他 task の entry を引き取ったり状態を書き換えたりしない (期限切れの確定も指定 entry のみ)。`take_ready` は待ち手が 1 つの場合の API として残した
- 上限は「積む前」に判定する。draft-ietf-moq-transport-22 §3.1.3.1 の "buffer it briefly" の上限管理であり、合計が `per_session_max_bytes` を超えないことを保証する。超えるチャンクは積まずにその entry を破棄し、他の entry は巻き込まない
- 破棄済みの entry にチャンクを足さない (足すと破棄したはずのバイトが集計に残り、無関係な stream を巻き込む)。上限超過で破棄した entry は、所有者が観測して解放できるよう `take_ready` / `take_ready_for` が理由付きで返す (moqt-js が上限超過でも通知するのと同じ)。`None` は未登録・`remove` 済み・未通知・引き取り済みに限る
- 通知の理由は最初の 1 回だけ確定する。`Subscriber` と `Timeout` で引き取った entry は所有者がまだチャンクを持ち得るため破棄とは扱わない (`remove` に委ねる)
- `examples/moq-sub/src/pipeline.rs` は、session (run) スコープで 1 つのバッファを共有し、各 stream task が自分の entry の識別子を保持して `take_ready_for` で自分の通知だけを引き取る。これで per-stream と per-session の両方の上限が意図どおり働く
- `UnknownTrackAlias` の Subgroup ストリームはチャンクを積みながら再試行し、購読が確立したら `SubgroupStreamDecoder` へ積み直して通常の復号経路に戻る。時間切れと上限超過は warn、session の終了とストリームの終端は debug を出して破棄する。`Discarded` / `FilteredOut` は保持せず捨てる (保持すると不要なデータが上限を圧迫する)
- テスト: `tests/test_pending_subgroup_buffer.rs` に 26 件、`pbt/tests/prop_pending_subgroup_buffer.rs` に 1 件 (理由 6 種と破棄・無視・期限切れ・引き取り・期限ちょうどの境界のカバレッジゲート付き)、example のテストを更新した
- 検証: `cargo fmt --check` / `make pbt` / `make test` (61 バイナリすべて成功) / `cargo clippy --workspace --all-targets -- -D warnings` / `RUSTDOCFLAGS="-D warnings" cargo doc` / no_std ビルドが成功した。`PBT_SEED` を 13 種類変えても property が成功する
- 検証: 13 種類の欠陥 (積む前の上限判定の除去 / 破棄のときに集計を戻さない / 期限切れの確定の除去 / 期限ちょうどの境界 / 引き取り済みの印を立てない / 引き取りで集計を戻さない / `remove` で集計を戻さない / `note_subscriber` が全 alias を解放する / 期限の確定を全 entry にする / `take_ready_for` が識別子を無視する / 破棄済みに `None` を返す / 破棄済みにチャンクを足す / 他の stream を巻き込む) を入れて、追加したテストと property がそれぞれ検出することを確認した
- 備考: この example は受信ループの開始前に購読を確立するため、`note_subscriber` を実行時に呼ぶ経路が無く、購読の確立は再試行の受理で検出する。example の `UnknownTrackAlias` は恒久的に未知のことが多く、実運用の主経路は 5 秒の時間切れによる warn 破棄である (保留したチャンクを読み直す再開経路は、実行中に購読を追加したときに効く)

# Track Alias 未確立の Subgroup ストリームを短時間保持するバッファを追加する

- Created: 2026-10-10
- Completed: {YYYY-MM-DD}
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

{未着手}

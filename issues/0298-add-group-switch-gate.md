# 前の Group の stream が開いている間、次の Group の映像 Object を保留する

- Created: 2026-10-17
- Completed: {YYYY-MM-DD}
- Branch: feature/add-group-switch-gate

## 目的

moqt-js は `src/groupSwitchGate.ts` (`GROUP_SWITCH_HOLD_MS` = 50) で、前の Group の stream が
開いている間、次の Group の映像 Object を保留する。次の Group のキーフレームを先に復号して
しまうと、遅れて届いた前の Group の末尾が `VideoDecodeOrder` に古い Group として捨てられ、
表示されない。GOP の末尾が 1〜数枚欠け、30 fps で 33〜100 ms の停止として見える。

moqt-rs には待ち合わせが無く、同じ欠落が起きる。`src/video_decode_order.rs` の `admit` が
「復号中の Group より古い Group」を `Stale` にし、`examples/moq-sub/src/pipeline.rs` の
`deliver_video_object` がその Object を `frame_tx` へ送らない。前の Group の stream を待つ
箇所は無い (`pending_subgroup_buffer` は Track Alias 未確立の stream 用であり、Group の順序
とは無関係である)。

## 現状

- 発生経路は 2 つある。(a) 前の Group の末尾が損失で再送されている間 (stream の FIN が届か
  ない) に、次の Group のキーフレームが届いて復号が進む。(b) 前の Group の stream タスクが
  遅れている間に、次の Group のタスクが走る (1 stream = 1 タスク、同時実行は 4 本)。
- 配信側は前の Group の stream を FIN してから次を開くため窓は狭いが、ゼロではない。
- 表示の順序そのものは乱れない (古い Group を落とすため、`frame_tx` は Group の順に進む)。

## 設計方針

- moqt-js と同じく、次の Group の Object を短時間 (50 ms) 保留する gate をライブラリに置く。
  Sans-I/O とし、時刻は引数で受ける。
- 保留の期限を過ぎたら保留を解いて復号へ進める。期限の駆動は example 側で行う
  (`examples/moq-sub/src/pipeline.rs` は周期 tick を持たないため、読み取りのたびに期限を
  確かめる)。
- 配線は live subgroup 経路と FETCH 応答経路の 2 箇所になる。
- 判断が要る: 前の Group の末尾 (33〜100 ms の停止) を取るか、Group の切り替えごとの最大
  50 ms の復号保留を取るか。moqt-js は保留を jitter buffer の再生遅延 (40 ms 程度) の範囲と
  して許容している。moq-sub はカタログの `targetLatency` (既定 200 ms) と時間軸で表示する
  ため、50 ms の保留は表示の時刻に間に合う見込みである。この判断が付くまで実装に着手しない
  (`issues/pending/` に置く理由でもある)。

## 完了条件

- 判断 (保留を入れるか) を issue に記録すること。
- 入れる場合: ライブラリに gate を追加し、前の Group の stream が開いている間は次の Group の
  Object を復号へ渡さないこと、期限を過ぎたら保留が解けることをテストで固定すること。
- 配線した example で、Group の切り替えで前の Group の末尾が表示されることをテストで固定
  すること。
- `make test` / `make clippy` / `make fmt` / `make pbt` が成功すること。
- ソースコードに issue 番号や issue への言及を書かないこと。

## 調査の記録

- moqt-js の実装: `src/groupSwitchGate.ts` (gate 本体)、`src/createMediaSubscriber.ts`
  (配線)。保留の長さは `groupSwitchGate.ts` の定数に、理由 (再生の遅延の範囲に収まること) が
  書かれている。
- moqt-rs で同じ害が出る根拠: `src/video_decode_order.rs` の `admit` の `Stale` 判定と、
  `examples/moq-sub/src/pipeline.rs` の `deliver_video_object` の送出条件。
- 似ているが別の issue: 0280 は購読の境界 (`SUBSCRIBE_OK` の Largest Object) 以前を表示
  しない話であり、本 issue の Group の切り替えの保留とは別である。

## pending にした理由

前の Group の末尾の欠落 (33〜100 ms の停止) と、Group の切り替えごとの最大 50 ms の復号保留
のどちらを取るかは、映像の見え方と表示の遅延の交換であり、実装だけでは決められない。moqt-js
は保留を選んでいるが、moqt-rs の example は `targetLatency` (既定 200 ms) と時間軸で表示
する構成であり、同じ判断がそのまま当てはまるかを確かめたうえで決める必要がある。判断が付く
まで実装に着手しないため `issues/pending/` に置く。

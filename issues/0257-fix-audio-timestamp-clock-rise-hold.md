# 配信側の TIMESTAMP 補正が一瞬の読み出し遅れで動くのを直す

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-audio-timestamp-clock-rise-hold
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の `src/audioTimestampClock.ts` は 2026-10-10 に、読み出しが一瞬遅れただけで補正が
動くのを直した。

`AudioTimestampClock` は「読み出した壁時計 − 音声の TIMESTAMP」の 2 秒の窓の最小値を補正と
して TIMESTAMP に足す。読み出しが一瞬遅れると、窓がその遅れで入れ替わって最小値が上がる。
すると送る TIMESTAMP が遅れの分 (実測 95 ms) だけ動き、受信側はこれを時計のずれとみなして
A/V の基準の共有を 30 秒解除する (CI の実リレーテストで起きた)。

moqt-rs の `src/audio_clock.rs` にも同じ経路がある。段差とみなす閾値 (200 ms) は「読み出しの
遅れ」と「時計のずれ」を分けておらず、閾値未満の遅れが 2 秒続けば窓の最小値が上がって補正が
動く。

## 現状

- `AudioTimestampClock::record` は毎回 `update_applied_offset` を呼び、2 秒の窓
  ([`AUDIO_TIMESTAMP_OFFSET_WINDOW_US`]) の最小値へ補正を合わせる
- 段差の取り直しは、直近 0.5 秒の窓が [`AUDIO_TIMESTAMP_OFFSET_STEP_MIN_SAMPLES`] 個以上
  あり、適用中の補正より [`AUDIO_TIMESTAMP_OFFSET_STEP_US`] (200 ms) 以上上にあるときだけ
  起きる。この条件に当たらない遅れは、2 秒の窓が入れ替わるにつれて補正を上げる
- 補正が上がると、配信側が送る TIMESTAMP が実際より新しくなり、受信側は配信元の時計が
  飛んだものとして基準の共有を解除する (`TIMELINE_BASE_UNSHARED_HOLD_US` の 30 秒)
- 読み出しの遅れが定着した場合 (機械が遅くなった、読み出しの経路が変わった) は、いずれ
  合わせないと送る TIMESTAMP が実際より古いままになり、受信側の基準の遅れがその分だけ
  伸びる

## 設計方針

moqt-js の最新 (2026-10-10) の判定を移植する。

- `AUDIO_TIMESTAMP_OFFSET_RISE_CHECK_US` (0.5 秒) を追加する。床の上がり方を見る直近の窓
- `AUDIO_TIMESTAMP_OFFSET_MAX_CLOCK_RISE_US_PER_SECOND` (100 ms/秒) を追加する。時計の
  ドリフトとして許す速さ。実測したドリフト (20〜50 ms/秒) の 2 倍である
- `AUDIO_TIMESTAMP_OFFSET_RISE_HOLD_US` (5 秒) を追加する。速すぎる上昇を保留する長さ。
  2 秒の窓が遅れで入れ替わる分と合わせ、読み出しの遅れが 7 秒未満なら動かない長さである
- 床が上がるとき (窓の最小値が適用中の補正より大きいとき) の分岐を足す
  - 保留中なら、保留から [`AUDIO_TIMESTAMP_OFFSET_RISE_HOLD_US`] 未満の間は何もしない
  - 保留していなければ、上がり方が速すぎるか調べ、速すぎるなら保留を始めて何もしない
- 上がり方が速すぎるかは、`[読んだ時刻 − (2 秒 + 0.5 秒), 読んだ時刻 − 0.5 秒]` の窓の
  最小値との差が `100 ms/秒 × 0.5 秒` を超えるかで決める。窓が無ければ速くないとする
- 下がる方向と段差の取り直しは従来どおり即座に合わせる。段差の取り直しと `reset` では
  保留を消す
- 状態に `rise_hold_since_us` を追加する
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 一過性の読み出しの遅れ (95 ms が 3 秒続いて戻る) では補正を動かさないことがテストで
  固定されていること
- 読み出しの遅れが定着したら、保留の長さを待った後でその水準へ合わせることがテストで
  固定されていること
- 段差 (200 ms 以上) の取り直しは待たずに起きることがテストで固定されていること
- 床が下がるときは即座に合わせることがテストで固定されていること
- `pbt/tests/prop_audio_clock.rs` が新しい契約に追随し、補正が観測の範囲に収まることを
  検証していること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)

# moq-sub の映像描画を実測し、描画停止から復帰する

- Created: 2026-10-21
- Completed: 2026-10-21
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

moq-sub が映像を実際に描画できているかを外から判断できず、描画が止まってもログと表示実績
だけが進む問題を修正する。あわせて、raw_player の映像のみ再生で再生時計が到着 PTS から
ずれたときに描画が止まり続ける問題から復帰できるようにする。

## 現状

- `run_raw_player` は `enqueue_video_i420` した時点で `frame_count` を増やし、
  `Rendered ... video frames` とログし、`PlayoutTimeline::record_presentation` にも投入
  時刻を記録していた。実際の描画は `poll_events` の中で起きるため、描画が止まっていても
  ログと表示実績だけが進む
- raw_player 2026.2 の映像のみ再生経路は、最初の描画時にキュー先頭フレームの PTS を
  SDL の単調時計へ一度だけ対応付ける。アプリ側の処理停滞や最初の `poll_events` の遅れで
  時計と到着 PTS の差が 40 ms を超えて固定されると、全フレームが待機か破棄の対象になり
  描画が止まり続ける。映像キューの上限は 5 枚である
- 1 回の実測では、プレイヤーへの投入 2369 枚に対して描画 11 枚、破棄 2353 枚だった。
  ログ上は投入だけが進んでいた
- raw_player 2026.3.0-canary.1 で、描画できない状態が 1 秒続いたら最新フレームへ復帰する
  処理と、破棄理由別のカウンタが追加された

## 変更対象

- `examples/moq-sub/src/main.rs`
- `examples/moq-sub/src/error.rs` (raw_player のモジュールパス化に伴う import の修正)
- `examples/moq-sub/Cargo.toml` / `Cargo.lock` (raw_player の更新)
- `examples/moq-sub/README.md`
- `docs/video-playback-stall.md`

## 設計方針

- `poll_events()` の直後に `stats()` を読み、描画数・破棄数の差分から実際に描画された
  フレームを求める。投入したフレームの TIMESTAMP は FIFO で対応付け、描画を観測した時点で
  `PlayoutTimeline::record_presentation` に記録する。TIMESTAMP の無いフレームは位置合わせ
  のため `None` として FIFO に積み、記録からは除く
- 投入時のキュー上限超過は FIFO の先頭を同時に除いておき、次回の破棄差分で二重に除かない
  ようにする。解像度変更でプレイヤーを作り直したときは基準値と FIFO をリセットする
- 描画が 1 秒以上進まず、その間に 2 枚以上投入している場合は `drain_video()` で復帰する。
  raw_player 2026.3 では自力で復帰するが、それより前のプレイヤーでも動くよう保険として残す
- ログは投入数・描画数・破棄数 (キュー上限超過、遅延、復帰時の内訳)・キュー長・復帰回数を
  分けて出す。復帰回数は moq-sub 側の `drain_video` と raw_player 側の再基準化を区別する
- 実測した描画時刻だけを表示実績に使うことで、A/V 同期のずれの計測に嘘の映像表示時刻を
  入れない

## 完了条件

- 描画・破棄・キュー上限超過の FIFO 対応付け、停止判定の境界、復帰後のリセットを SDL を
  使わない単体テストで検証すること
- ログに `enqueued` / `rendered` / `dropped` (内訳として `queue_full` / `late` /
  `resync`) / `queue` / `drain_recoveries` / `player_resyncs` が出ること
- `cargo fmt --all --check` / `cargo clippy -p moq-sub --all-targets -- -D warnings` /
  `cargo test -p moq-sub` が通ること
- raw_player 2026.3.0-canary.1 でビルドできること

## 解決方法

1. `run_raw_player` で `poll_events()` の直後に `stats()` を読み、描画数・破棄数の差分から
   実際に描画されたフレームを求める `VideoPlayoutAccounting` を追加した。投入したフレーム
   の TIMESTAMP は FIFO で対応付け、描画を観測した時点で `PlayoutTimeline` に記録する。
   TIMESTAMP の無いフレームは位置合わせのため `None` として積み、記録からは除く
2. 投入時のキュー上限超過は FIFO の先頭を同時に除き、次回の破棄差分で二重に除かないように
   した。解像度変更でプレイヤーを作り直したときは基準値と FIFO をリセットする
3. 描画が 1 秒以上進まず、その間に 2 枚以上投入している場合は `drain_video()` で復帰し、
   警告ログと復帰回数を出す。ログでは moq-sub 側の復帰 (`drain_recoveries`) と raw_player
   側の再基準化 (`player_resyncs`) を区別する
4. ログを実測値に変え、毎秒 `enqueued` / `rendered` / `dropped` (内訳として `queue_full` /
   `late` / `resync`) / `queue` を出す。投入数を描画数として出す `Rendered ... video frames`
   は削除した
5. raw_player を 2026.3.0-canary.1 に更新し、モジュールパス化に合わせて import を修正した。
   canary には描画が止まったときに最新フレームへ復帰する処理と破棄理由別カウンタが入って
   いる
6. 単体テストを 8 本追加し、描画・破棄・キュー上限超過の FIFO 対応付け、停止判定の境界、
   復帰後のリセットを検証する

検証は `cargo fmt --all --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test -p moq-sub` / `cargo test --workspace` がすべて成功している。

# moq-pub の音声の配信へ追いつきの判定を配線する

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/wire-publish-catch-up
- Polished: {YYYY-MM-DD}

## 目的

`src/playout/publish_catch_up.rs` に入れた「遅れたフレームを捨てて live へ戻す」判定を、
`examples/moq-pub` の音声の配信へ配線する。今は判定が置いてあるだけで、実際の配信では
使われていない。符号化が実時間に追いつかなくなるとキューが溜まり、遅れが受信側の基準を
押し上げて A/V の基準の差を開かせる。

## 現状

- `examples/moq-pub/src/pipeline.rs` の音声のループは、読み出したフレームをそのまま
  符号化へ投入する。キューが溜まっても捨てる仕組みが無い
- 判定 (`AudioPublishCatchUp`) は `src/playout/publish_catch_up.rs` にあり、テストと PBT で
  契約が固定されている
- 読み出しの遅れを求めるには、チャンクの TIMESTAMP と読んだ壁時計が要る。TIMESTAMP は
  live capture では音声の時計の補正を通した epoch マイクロ秒であり、再エンコードでは
  入力のタイムスケールである
- チャンクの長さはサンプル数から求める (`samples_to_micros`)

## 設計方針

- 音声のループの外で `AudioPublishCatchUp` を作る (配信ごとに 1 つ)
- チャンクの TIMESTAMP を先に決めてから判定する。今は符号化の後に決めているため、順序を
  入れ替える
- `evaluate` が偽を返したら符号化へ渡さずにそのチャンクを捨て、捨てた数と長さをログに出す
- `evaluate` が真を返したら符号化へ渡し、`record_encoded_chunk` で出力を記録する
- 読み出しの遅れは「読んだ壁時計 − TIMESTAMP」とする。TIMESTAMP は既に補正を通した値で
  あるため、補正は 0 として渡す
- 再エンコードでは TIMESTAMP が入力のタイムスケールであり壁時計ではない。読み出しの遅れを
  測れないため、判定をしない (`applied_offset_us` を `None` にする)
- 方針 (`drop` / `keep`) は既定 (`drop`) とする
- フェード (棄却で飛んだ境目を繋ぐ `src/playout/splice_fade.rs`) の配線は、判定の配線とは
  分けて別の issue で行う

## 完了条件

- 健全な状態ではすべてのチャンクが符号化へ渡ることがテストで固定されていること
- キューが伸びたら符号化へ渡さないチャンクが出ることがテストで固定されていること
- 捨てた数と長さがログに出ること
- 再エンコードでは判定をしないことがテストで固定されていること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)

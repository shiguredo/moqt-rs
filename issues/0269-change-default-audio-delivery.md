# 音声の配送を OBJECT_DATAGRAM 既定にする

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/change-default-audio-delivery
- Polished: {YYYY-MM-DD}

## 目的

moqt-js の音声の配信の改善を取り込む最後の 1 件である。moqt-js は音声の配送を
OBJECT_DATAGRAM で行う。moqt-rs の例は subgroup 配送が既定であり、`--audio-datagram` を
指定したときだけ datagram になる。既定を datagram にする。

draft-ietf-moq-loc-04 §4.1 (Application with one audio track) は「1 audio frame =
1 Object = 1 Group」と定めており、datagram 配送が素直である。datagram は再送されないため、
遅れた音声を届けるより落とす方が A/V のずれを残さない。追いつきの判定
(`src/playout/publish_catch_up.rs`) もその前提で作ってある。

## 現状

- `examples/moq-pub/src/cli.rs` の `audio_datagram` は `noargs::flag("audio-datagram")` で
  既定が無効である。subgroup 配送が既定になる
- `examples/moq-sub` はどちらの配送でも受信できる (datagram の受信は 0253 で追加した)
- 実 relay の OBJECT_DATAGRAM 配送は動作を確認済みである

## 設計方針

- `--audio-subgroup` を追加し、`audio_datagram` の既定を真にする
- `--audio-datagram` は削除する (後方互換は不要)
- 警告メッセージ (`--audio-datagram is ignored ...`) を `--audio-subgroup` に合わせる
- `--input-mp4` (再エンコード) では音声を publish しないため、どちらの配送も使わない
- `examples/moq-pub/README.md` の記述を既定に合わせる
- E2E テストの publisher から `--audio-datagram` を外す (既定で datagram になる)
- 後方互換は不要 (破壊的変更を恐れない)

## 完了条件

- 既定で datagram 配送になることがテストで固定されていること
- `--audio-subgroup` で subgroup 配送になることがテストで固定されていること
- `--audio-subgroup` と `--input-mp4` の組み合わせが警告になることがテストで固定されて
  いること
- E2E Test が成功すること
- `make pbt` / `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)

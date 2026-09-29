# moq-pub の再エンコード配信で編集リスト (elst) を適用しないため A/V がずれる

- Created: 2026-09-29
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-mp4-reencode-edit-list
- Polished: {YYYY-MM-DD}

## 目的

`moq-pub --input-mp4-reencode` は編集リスト (elst) を適用せず、トラック内の 0 起点の DTS をそのまま LOC の Timestamp にする。編集リストを持つ MP4 (例: ffmpeg が生成した AV1 + Opus) では、編集リストのオフセット分だけトラック間の表示開始位置がずれ、A/V がずれる。

## 現状

- `examples/moq-pub/src/mp4/reencode.rs` の `compute_pts` はサンプルの `timestamp` と `composition_time_offset` から PTS を求め、編集リストは考慮しない。音声も先頭サンプルのタイムスタンプを起点にするだけである。
- `shiguredo_mp4` 2026.5.0 の `Mp4FileDemuxer` は編集リストを公開しない。トラック情報の `duration` (mdhd) も編集リストを含まない。
- ffmpeg で生成した AV1 + Opus の MP4 に `elst` ボックスが含まれることを実測で確認している。
- `moq-sub --mp4` の録画は編集リストを書かないため、往復確認では影響しない。
- `issues/closed/0180-add-publisher-mp4-reencode.md` の設計方針で「編集リストの適用が必要になった時点で別 issue とする」としていた。

## 設計方針

- 次のいずれかを実測 (ffmpeg 生成 MP4 を moq-sub で再生した際のずれ量) を踏まえて決める。
  - `shiguredo_mp4` に編集リストを公開する API を追加し、トラックごとの `media_time` / `segment_duration` を PTS に適用する。
  - `MoovBox` を直接デコードして編集リストを取り出し、トラックごとの開始オフセットとして PTS に反映する。
  - 編集リストを適用できない場合は、編集リストを持つ MP4 を検出して警告する (ずれが起きることを利用者に伝える)。
- 適用する場合の正本は編集リストの `media_time` とし、トラック間の相対関係 (A/V の開始位置) を揃える。

## 完了条件

- 編集リストを持つ MP4 (ffmpeg 生成の AV1 + Opus) で A/V のずれが生じない、または検出して警告する。
- 編集リストの適用 / 検出が単体テストまたは実機確認 (moq-sub での再生) で固定されている。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

{対応後に追記する}

# moq-pub の再エンコード配信で編集リスト (elst) を適用しないため A/V がずれる

- Created: 2026-09-29
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-mp4-reencode-edit-list
- Polished: 2026-09-29

## 目的

`moq-pub --input-mp4-reencode` は編集リスト (elst) を適用せず、映像はトラック内 0 起点の DTS と composition_time_offset から求めた PTS をそのまま LOC の Timestamp にする。B フレームを含む映像トラックを持つ MP4 (例: ffmpeg が生成した H.264 + Opus) では、編集リストの `media_time` (先頭フレームの表示オフセット) 分だけ映像が音声より遅れて提示され、A/V がずれる。なお、ffmpeg が生成した AV1 + Opus のように映像の `media_time` が 0 の MP4 では実際にはずれない (後述の実測)。

## 現状

- `examples/moq-pub/src/mp4/reencode.rs` の `compute_pts` はサンプルの `timestamp` と `composition_time_offset` から PTS を求め、編集リストは考慮しない。音声も先頭サンプルのタイムスタンプを起点にするだけである。
- `shiguredo_mp4` 2026.5.0 の `Mp4FileDemuxer` は編集リストを公開しない。トラック情報の `duration` (mdhd) も編集リストを含まない。一方、編集リストそのものの型 (`MoovBox` / `TrakBox.edts_box` / `EdtsBox.elst_box` / `ElstEntry`) は `shiguredo_mp4::boxes` で公開されている (ただし `ElstEntry.media_time` は media timescale 単位、`edit_duration` は movie timescale 単位の二重 timescale である)。
- 実測 (ffmpeg 6.1.1、libaom-av1 / libsvtav1 + libopus):
  - 生成した AV1 + Opus の MP4 にはトラックごとに 1 エントリの `elst` が入る。映像は `media_time=0`、音声は `media_time=312` (= `dOps` の pre_skip 312、48 kHz 単位)。moq-pub は音声の pre_skip を読み飛ばして先頭タイムスタンプを 0 にし、映像も PTS 0 起点になるため、この MP4 では A/V はずれない。
  - B フレームを含む H.264 + Opus (x264 + libopus) の MP4 では映像の `elst` が `media_time=1024` (timescale 15360、約 66.7 ms、先頭フレームの cts offset と一致) になる。moq-pub は映像の先頭 PTS にこの 1024 をそのまま使い、音声は 0 起点のため、映像が約 66.7 ms 遅れて A/V がずれる。B フレームを含む H.265 も同様である。
- `moq-sub --mp4` の録画は編集リストを書かないため、往復確認では影響しない。
- `issues/closed/0180-add-publisher-mp4-reencode.md` の設計方針で「編集リストの適用が必要になった時点で別 issue とする」としていた。

## 設計方針

- 次のいずれかで対応する。ずれ量の実測は「現状」に記録済みである。
  - `shiguredo_mp4` に編集リストを公開する API を追加し、トラックごとの `media_time` を PTS に適用する (shiguredo_mp4 の変更と新規リリース、本リポジトリの依存更新を伴う)。
  - `MoovBox` を `shiguredo_mp4::boxes` から直接デコードして編集リストを取り出し、トラックごとの開始オフセットとして PTS に反映する (例: `MoovBox` / `EdtsBox` / `ElstBox` / `ElstEntry` を利用する。moq-pub の実装のみで完結する)。
  - 編集リストを適用できない場合は、編集リストを持つ MP4 を検出して警告する (ずれが起きることを利用者に伝える)。
- 適用する場合の正本は編集リストの `media_time` (media timescale 単位) とし、トラック間の相対関係 (A/V の開始位置) を揃える。`edit_duration` は movie timescale 単位であるため PTS の適用対象にせず、単位の取り違えに注意する (ループ周期を検討する場合も同様)。
- 音声トラックは既存の Opus pre-skip 読み飛ばし (RFC 7845 §4.2) により、`media_time` (= `dOps` の pre_skip) のシフトが実質適用済みである。編集リストを適用する際は音声へ二重にシフトしないこと。適用前後で音声のタイムスタンプ系列が変わらないことをテストまたは実機確認で固定する。

## 完了条件

- 映像トラックの `media_time` が 0 でない MP4 (例: ffmpeg 生成の H.264 + Opus) で、映像と音声の開始時刻が揃い A/V のずれが生じない、または編集リストを適用できない場合は検出して警告する。H.264 / H.265 のデコードは macOS 限定のため、それ以外の環境では同条件の他の MP4 (B フレームを含む AV1 など) で代替確認する。
- 編集リストの適用 / 検出が単体テストまたは実機確認 (moq-sub での再生) で固定されている。音声のタイムスタンプ系列が編集リスト適用の前後で変わらないことも固定する。
- `make test` / `make clippy` / `make fmt` が通る。

## 解決方法

{対応後に追記する}

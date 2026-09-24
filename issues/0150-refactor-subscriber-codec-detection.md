# moqt-subscriber の codec 種別判定を library の判定に寄せる

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-subscriber-codec-detection
- Polished: {YYYY-MM-DD}

## 目的

`shiguredo_moqt` は codec 文字列から audio / video を判定する規則を `src/msf.rs` に持つが、`examples/moqt-subscriber/src/pipeline.rs` は独自の前方一致で同じ判定を行っている。両者は対象 codec も境界規則もずれており、library が video と判定するトラックを example が video として扱わない。判定規則を一本化する。

## 現状

`examples/moqt-subscriber/src/pipeline.rs` の `receive_catalog` は `starts_with("av01")` / `starts_with("avc1")` / `starts_with("hvc1")` / `starts_with("hev1")` で video を判定し、`starts_with("opus")` で audio を判定している。同じ形の判定が複数箇所に現れる。

library 側 (`src/msf.rs` の `is_audio_codec` / `is_video_codec`) は登録名の完全一致と区切り文字境界付き前方一致で判定し、対象も `avc3` / `vp8` / `vp09` / `flac` / `mp3` / `vorbis` / `ulaw` / `alaw` / `mp4a.*` / `pcm-*` を含む。両者は一致していない。

## 設計方針

判定規則を library に一本化し、example 側に判定表を複製しない。

- `shiguredo_moqt` に codec 種別を返す公開 API を追加する (例: トラックまたは codec 文字列から audio / video / 不明を返す列挙型)。Sans I/O の性質は変えない
- `examples/moqt-subscriber` は追加した API で video / audio を判定し、example 内の前方一致判定を削除する
- `examples/moqt-publisher` の codec 文字列は変更しない
- 挙動が変わる点を明確にする。library は `avc3` / `vp8` / `vp09` も video、`flac` 等も audio と判定するため example の受信対象が広がる

## 完了条件

- `shiguredo_moqt` に codec 種別の公開 API が追加され、テストで固定されていること
- `examples/moqt-subscriber/src/pipeline.rs` から codec 文字列の前方一致判定が消えていること
- 追加した API が library の種別判定と同じ結果を返すこと
- `make test` / `make clippy` / `make fmt` が通ること

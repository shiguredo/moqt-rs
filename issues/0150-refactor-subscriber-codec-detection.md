# moq-sub の codec 種別判定を library の判定に寄せる

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/refactor-subscriber-codec-detection
- Polished: 2026-09-28

## 目的

`shiguredo_moqt` は codec 文字列から audio / video を判定する規則を `src/msf.rs` に持つが、`examples/moq-sub/src/pipeline.rs` は独自の前方一致で同じ判定を行っている。両者は対象 codec も境界規則もずれており、library が video と判定するトラック (例: `vp09` / `vp8` / `avc3`) を example が video として扱わない。audio / video の種別判定規則を library に一本化する。

## 現状

`examples/moq-sub/src/pipeline.rs` の `receive_catalog` は `starts_with("av01")` / `starts_with("avc1")` /
`starts_with("hvc1")` / `starts_with("hev1")` を満たす最初のトラックを video、`starts_with("opus")` を満たす最初のトラックを
audio として選択する。同じファイルの `handle_incoming_stream` にも `audio_codec` に対する `starts_with("opus")` があり、
OpusHead パースの要否判定に使っている。`decoder.rs` の `build_video_decoder` も `starts_with` で codec を分岐しているが、
こちらはデコーダ選択であり audio / video の種別判定ではない。

library 側 (`src/msf.rs` の `is_audio_codec` / `is_video_codec`) は登録名の完全一致と区切り文字境界付き前方一致で判定し、対象も `avc3` / `vp8` / `vp09` / `flac` / `mp3` / `vorbis` / `ulaw` / `alaw` / `mp4a.*` / `pcm-*` を含む。両者の対象 codec と境界規則は一致していない。

## 設計方針

audio / video の種別判定を library に一本化し、example 側に種別判定表を複製しない。

- `shiguredo_moqt` に codec 種別を返す公開 API を追加する (例: codec 文字列から audio / video / 不明を返す列挙型)。Sans I/O の性質は変えない
- `examples/moq-sub` は追加した API で audio / video の種別を判定し、`receive_catalog` 内の前方一致による種別判定を削除する
- 種別判定と選択は分ける。library API で video / audio と判定されたトラックのうち、example がデコードできるものだけを選択する。example のデコーダは video が `av01` / `avc1` / `hvc1` / `hev1` (`decoder.rs` の `build_video_decoder`)、audio が `opus` (Opus デコーダのみ) に対応しており、デコードできない codec のトラックは選択しない
- `decoder.rs` の codec 分岐 (`starts_with`) と、OpusHead パースの要否判定 (`starts_with("opus")`) は、種別判定ではなく codec 実装の選択であるため残す
- `examples/moq-pub` の codec 文字列は変更しない
- 挙動が変わる点を明確にする。種別判定は library と一致し、`vp09` / `vp8` / `avc3` や `flac` 等も種別として認識されるが、デコードできない codec のトラックは example の選択対象にならない。デコード可能な codec のトラックでは従来と選択結果は変わらない

## 完了条件

- `shiguredo_moqt` に codec 種別の公開 API が追加され、テストで固定されていること
- `examples/moq-sub/src/pipeline.rs` の `receive_catalog` から codec 文字列の前方一致による audio / video 種別判定が消え、library の API を使うこと
- 追加した API が library の種別判定 (`src/msf.rs` の `is_audio_codec` / `is_video_codec`) と同じ結果を返すこと
- 種別判定の置き換え後も、example がデコードできる codec のトラックを選択し、デコードできない codec だけのカタログでは該当する video / audio トラックを選択しないこと
- `make test` / `make clippy` / `make fmt` が通ること

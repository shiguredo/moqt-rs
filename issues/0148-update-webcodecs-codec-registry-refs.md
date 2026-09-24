# WEBCODECS-CODEC-REGISTRY を refs/ に保存する

- Created: 2026-09-23
- Completed: {YYYY-MM-DD}
- Branch: feature/update-webcodecs-codec-registry-refs
- Polished: {YYYY-MM-DD}

## 目的

MSF の codec 判定は draft-ietf-moq-msf-01 §5.2.18 が参照する WEBCODECS-CODEC-REGISTRY の登録名に依存しているが、レジストリの写しが `refs/` に無いため判定表の根拠をリポジトリ内で検証できない。他の一次資料と同じく `refs/` に保存し、判定表を更新するときに差分照合できるようにする。

## 現状

`refs/` には moq / h3 / quic / webtrans の IETF draft と RFC のテキストが保存されている。WEBCODECS-CODEC-REGISTRY は保存されておらず、`src/msf.rs` の `is_audio_codec` / `is_video_codec` のコメントに URL と版 (`Registry Draft, 2026-02-12`) と節 (§3 / §4) を書くに留まる。

判定表が持つ登録名は次のとおり。

- audio: `flac` / `mp3` / `mp4a.*` / `opus` / `vorbis` / `ulaw` / `alaw` / `pcm-*`
- video: `av01.*` / `avc1.*` / `avc3.*` / `hev1.*` / `hvc1.*` / `vp8` / `vp09.*`

## 設計方針

- W3C の Registry Draft (2026-02-12 版) のテキストを取得し、`refs/` 配下の新しいディレクトリ (例: `refs/webcodecs/`) に保存する
- ファイル名は他の refs に合わせ、版が分かる名前にする
- `src/msf.rs` の判定表コメントから保存先を辿れるようにする
- 保存したテキストの登録名と現行の判定表が一致することを確認する
- レジストリは "Existing entries cannot be deleted or deprecated." と定めるため、将来の更新時は追記差分だけを確認する運用にする

## 完了条件

- WEBCODECS-CODEC-REGISTRY のテキストが `refs/` 配下に保存されていること
- `src/msf.rs` の判定表が保存したレジストリの登録名と一致することを確認し、解決方法に記録していること
- 保存先と版がコードのコメントから辿れること
- `make test` / `make clippy` / `make fmt` が通ること

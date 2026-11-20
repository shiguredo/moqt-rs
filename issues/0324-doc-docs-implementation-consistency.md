# docs / README / SKILL.md / crates の記述を実装に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-user-facing-docs-consistency
- Polished: 2026-11-20

## 目的

docs と README / SKILL.md / crates の記述が実装と食い違い、利用者やエージェントに誤った API・挙動を案内している。実装を正として利用者向けの記述を修正する。コードの挙動は変えず、README のリンクは crates.io の crate ページでも解決するようにする。

## 現状

### docs/moqt.md

- 「モジュール構成」の表に公開モジュールが足りない
  - `src/lib.rs` で `pub mod` されている `audio_clock` / `media_clock` / `pending_subgroup_buffer` / `video_decode_order` / `video_object_reorder` / `group_switch_gate` が表に無い
  - `media_clock` は「アーキテクチャ」の表と「その他」には出てくるが、モジュール表には無い
- 「その他」の playout のモジュール一覧に `publish_catch_up` が無い
  - 「到着の揺らぎと実出力の先行量からの再生予算、鳴らす時刻の決定、A/V 同期の遅延制御、音声と映像の共通の時間軸、表示時刻に合わせたフレームの選択、時間圧縮・伸長と欠落の補間 (`playout::{timeline, scheduler, buffer, stretch, expansion, predictor, gap, splice_fade, resample, timing, catch_up}`)」
  - `src/playout.rs` は `pub mod publish_catch_up;` を持つ
- 「時間圧縮・伸長」は実装に合っている
  - `src/playout/stretch.rs` のモジュール doc は「波形の周期による音声の時間圧縮・時間伸長」と書き、`compress` と `preemptively_expand` を持つ
  - `docs/moqt.md` の「時間圧縮・伸長」はこの実装に合っているため変更しない (`src/lib.rs` の記述は 0321 が扱う)

### skills/shiguredo-moqt/SKILL.md

- 「モジュール構成」の表に公開モジュールが足りない
  - `src/lib.rs` で `pub mod` されている `audio_clock` / `media_clock` / `pending_subgroup_buffer` / `video_decode_order` / `video_object_reorder` が表に無い
  - `src/playout.rs` で `pub mod` されている `playout` のサブモジュール (`buffer` / `catch_up` / `expansion` / `gap` / `predictor` / `publish_catch_up` / `resample` / `scheduler` / `splice_fade` / `stretch` / `timeline` / `timing`) が表に無い
- `playout::timeline` の API 一覧に free function `audio_arrival_delay_us_with_min` が無い
  - 一覧は `audio_arrival_delay_us` / `sync_compensation_limit_us` / `playout_delay_percentile` / `playout_queue_cap_us` だけを挙げる
  - `src/playout/timeline.rs` は `pub fn audio_arrival_delay_us_with_min(learned_delay_us: i64, minimum_delay_us: i64) -> i64` を持つ
  - `observe_audio_playout` は現行の `PlayoutTimeline` に無い。`observe_audio_output_lead` と `observe_audio_arrival_delay` が観測を足す
- `playout::stretch` の API 一覧に `compress_with_history` が無い
  - 一覧は `compress` / `preemptively_expand` / `conceal` / `concealment_end_gain` / `is_time_stretch_available` を挙げる
  - `src/playout/stretch.rs` は `pub fn compress_with_history` を持つ
- `select_nearest` / `conceal` / A/V 同期の記述は現行実装と一致している
  - `select_nearest` は「`target - max_late` から `target + tolerance` の範囲で目標に最も近い 1 枚」を選び、それより古いフレームを捨てる (`src/playout/buffer.rs`)
  - `conceal` は `TIME_STRETCH_TAIL_CORRELATION_LEN` (39 サンプル、4 kHz で 9.75 ms) の固定長窓で末尾の周期を求める (`src/playout/stretch.rs`)
  - A/V 同期は、音声と映像の両方に基準があり `drifted_stream()` が無いときに同期し、音声の TIMESTAMP が信用できないときは映像だけを音声の到着基準へ合わせる (`src/playout/timeline.rs`)
  - 本 issue では変更しない

### crates の README / メッセージ

- `crates/moq-sub/README.md` が存在しない `--audio-datagram` を案内する
  - 「音声トラックは subgroup stream と datagram の両方で届きうる。LOC は音声について「1 audio chunk = 1 Object = 1 Group」の datagram 配送を例示しており (draft-ietf-moq-loc-05 §4.1)、moq-pub は `--audio-datagram` でこの配送を選べる。」
  - 実際は datagram 配送が既定であり、`--audio-subgroup` は subgroup stream への切り替えである (`crates/moq-pub/src/cli.rs` の `let audio_datagram = !audio_subgroup;`、`crates/moq-pub/README.md` の「既定は datagram」)
- `crates/moq-pub/src/error.rs` の datagram 上限超過メッセージが存在しない `--audio-datagram` を案内する
  - 「Disable --audio-datagram or increase --datagram-max-size」 (2 箇所。1 箇所はテストの期待値)
  - 正しい対処は `--audio-subgroup` で subgroup 配送に切り替えるか、`--datagram-max-size` を増やすことである
- `crates/moq-sub/src/pipeline.rs` の `audio_datagram_object_is_decoded_into_a_frame` の doc コメントが存在しない `--audio-datagram` を案内する
  - 「moq-pub は `--audio-datagram` で 1 audio chunk = 1 Object = 1 Group の datagram を送る」
  - 実際は既定で datagram を送る
- `crates/moq-pub/README.md` の `--audio-subgroup` の説明に `--datagram-max-size` の上限説明が混ざっている
  - 「上限は `--datagram-max-size` で指定し、超える object はエラーになる」
  - `--audio-subgroup` は datagram を使わないため、この説明は当てはまらない。`--datagram-max-size` の行に同じ説明がある

### README の相対リンク

- `README.md` は docs / crates への相対リンクを持つ
  - `docs/c4m.md` / `docs/audio-playout.md`
  - `crates/moq-pub/README.md` / `crates/moq-sub/README.md` / `crates/tokio-moq/README.md` / `crates/README.md`
- `Cargo.toml` の `include = ["/LICENSE", "/README.md", "/src/**"]` は docs / crates を含まない
- crates.io は package の README を crate ページで描画するが、package 内の docs / crates を URL として配信しない。`include` に追加しても README の相対リンクは解決しない
- docs.rs は rustdoc を表示するため README のリンクは対象外である
- 影響: crates.io の crate ページで README のリンクが壊れる

## 変更対象

- `docs/moqt.md`
- `skills/shiguredo-moqt/SKILL.md`
- `README.md`
- `crates/moq-sub/README.md`
- `crates/moq-pub/README.md`
- `crates/moq-pub/src/error.rs`
- `crates/moq-sub/src/pipeline.rs`

## 対象外

- `docs/missing-features.md` の `--audio-datagram` と playout の制限の記述は 0323 が扱う。本 issue では変更しない
- `src/lib.rs` の `playout` モジュール doc の「時間圧縮・伸長」は 0321 が扱う。`src/playout/scheduler.rs` の「時間伸縮」は時間圧縮と時間伸長の両方を指す一般的な表現であり、本 issue では変更しない
- `src/` と `crates/tokio-moq/src/` の公開 API doc の欠落補完は 0321 が扱う。コメントの言語・表記は 0322 が扱う
- `crates/tokio-moq/README.md` と `crates/tokio-moq/src/moqt_client.rs` の C4M の doc と README の列挙は既に FETCH を含むため変更しない
  - `crates/tokio-moq/README.md` は「PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE」
  - `crates/tokio-moq/src/moqt_client.rs` の `auth_tokens` と `establish_*` の doc は「PUBLISH / SUBSCRIBE / FETCH / REQUEST_UPDATE」
  - テストの「残る送信経路 (PUBLISH / SUBSCRIBE / REQUEST_UPDATE)」は、fixture が FETCH だけを認可するため FETCH を除いた残りを指しており、正しい
- `crates/moq-sub/src/pipeline.rs` の `run` の doc コメントは既に `pub async fn run` の直前にあり、`video_subscribe_parameters` の doc もその直前に分離されているため変更しない
- コードの挙動 (パース、配送、playout の計算) は変えない
- `Cargo.toml` の `include` に docs / crates を追加しない (README の絶対 URL 化で解決する)
- `CHANGES.md` は変更しない (CODEBASE.md の方針)

## 設計方針

- 実装の API と挙動から記述を再生成する。存在しないフラグ・古い API 名・実装と違う説明を削除または修正する
- `docs/moqt.md` の「モジュール構成」の表に `audio_clock` / `media_clock` / `pending_subgroup_buffer` / `video_decode_order` / `video_object_reorder` / `group_switch_gate` を追加する
- `docs/moqt.md` の「その他」の playout のモジュール一覧に `publish_catch_up` を追加する
- `docs/moqt.md` の「時間圧縮・伸長」は `src/playout/stretch.rs` の `compress` と `preemptively_expand` に合っているため変更しない
- `skills/shiguredo-moqt/SKILL.md` の「モジュール構成」の表に `audio_clock` / `media_clock` / `pending_subgroup_buffer` / `video_decode_order` / `video_object_reorder` と `playout` の公開サブモジュール (`buffer` / `catch_up` / `expansion` / `gap` / `predictor` / `publish_catch_up` / `resample` /
   `scheduler` / `splice_fade` / `stretch` / `timeline` / `timing`) を追加する
- `skills/shiguredo-moqt/SKILL.md` の `playout::timeline` の free function 一覧に `audio_arrival_delay_us_with_min` を追加する
- `skills/shiguredo-moqt/SKILL.md` の `playout::stretch` の一覧に `compress_with_history` を追加する
- `skills/shiguredo-moqt/SKILL.md` の `select_nearest` / `conceal` / A/V 同期の記述は現行実装と一致しているため変更しない
- `--audio-datagram` は存在しない。datagram 配送が既定であり、`--audio-subgroup` は subgroup stream への切り替えである。単純なフラグ名の置換をせず、意味が通る文に書き直す
  - `crates/moq-sub/README.md` は「moq-pub は音声を既定で datagram 配送し、`--audio-subgroup` で subgroup stream に切り替えられる」に直す
  - `crates/moq-pub/src/error.rs` は「Use --audio-subgroup or increase --datagram-max-size」に直す (2 箇所、テストの期待値も含む)
  - `crates/moq-sub/src/pipeline.rs` は「moq-pub は音声を既定で datagram 配送する (1 audio chunk = 1 Object = 1 Group)」に直す
- `crates/moq-pub/README.md` の `--audio-subgroup` の行から `--datagram-max-size` の上限説明を削除する
- `README.md` の docs / crates へのリンクを GitHub の絶対 URL (`https://github.com/shiguredo/moqt-rs/blob/develop/...`) にする。`Cargo.toml` の `include` は変更しない
- コードの挙動は変えない。既存の API 名・フラグ名を削除・置換するときは、実装の意味を確認してから書き換える

## 完了条件

- `rg -n -- '--audio-datagram' crates/` が 0 件であること
- `crates/moq-sub/README.md` に datagram 配送が既定であることと `--audio-subgroup` の意味が書かれ、`crates/moq-sub/src/pipeline.rs` に datagram 配送が既定であることが書かれ、`crates/moq-pub/src/error.rs` が `--audio-subgroup` を対処として案内していること
- `crates/moq-pub/README.md` の `--audio-subgroup` の行に `--datagram-max-size` の上限説明が無いこと
- `README.md` の docs / crates へのリンクが `https://github.com/shiguredo/moqt-rs/...` の絶対 URL であること (`rg -n '\]\((docs|crates|skills)/' README.md` が 0 件)
- `docs/moqt.md` の「モジュール構成」の表に `audio_clock` / `media_clock` / `pending_subgroup_buffer` / `video_decode_order` / `video_object_reorder` / `group_switch_gate` があり、「その他」の playout のモジュール一覧に `publish_catch_up` があること
- `skills/shiguredo-moqt/SKILL.md` の「モジュール構成」の表に `audio_clock` / `media_clock` / `pending_subgroup_buffer` / `video_decode_order` / `video_object_reorder` と `playout` の公開サブモジュールがあり、`playout::timeline` の free function 一覧に `audio_arrival_delay_us_with_min`、
  `playout::stretch` の一覧に `compress_with_history` があること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
- `markdownlint-cli2` が変更した Markdown で通ること

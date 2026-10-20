# docs / README / SKILL.md / examples の記述を実装に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/update-user-facing-docs-consistency
- Polished: 2026-10-20

## 目的

docs と README / SKILL.md / examples の記述が実装と食い違い、利用者やエージェントに誤った API・挙動を案内している。実装を正として利用者向けの記述を修正する。コードの挙動は変えず、README のリンクは crates.io の crate ページでも解決するようにする。

## 現状

### docs/moqt.md

- その他の節の playout の説明とモジュール表が「音声の時間圧縮・伸長」と書く
  - その他の節の playout の説明: 「音声の時間圧縮・伸長と欠落の補間、目標遅延の学習、鳴らす時刻の決定、A/V 同期の遅延制御、音声と映像の共通の時間軸、表示時刻に合わせたフレームの選択 (`playout::{stretch, delay, scheduler, timeline, buffer}`)」
  - モジュール表の `playout` 行: 「音声の時間圧縮・伸長と欠落の補間、目標遅延の学習、鳴らす時刻の決定、A/V 同期の遅延制御、音声と映像の共通の時間軸、表示時刻に合わせたフレームの選択」
  - `src/playout/stretch.rs` のモジュール doc は「時間伸長は持たない。この再生は音を伸ばす場面を持たないためである」と明記する。公開関数は `is_time_stretch_available` / `compress` / `concealment_end_gain` / `conceal` で、時間伸長の API は無い (公開定数は別に複数ある)

### skills/shiguredo-moqt/SKILL.md

- `description` / 特徴 / モジュール表が「時間伸縮」と書く一方、playout の詳細は「時間伸長は持たない」と書く
  - `description`: 「音声の再生補助 (playout: 時間伸縮・目標遅延・鳴らす時刻・A/V 同期・共通の時間軸)」
  - 特徴: 「**音声の再生補助**: 時間伸縮、目標遅延の学習、鳴らす時刻の決定、A/V 同期の遅延制御」
  - モジュール表の `playout` 行: 「時間伸縮、目標遅延の学習、鳴らす時刻の決定、A/V 同期の遅延制御、共通の時間軸」
  - 詳細: 「時間伸長は持たない (この再生は音を伸ばす場面を持たず、足りない区間は `conceal` が埋める)」
- `PlayoutBuffer::select` の説明が実装と一致しない
  - SKILL: 「表示時刻を過ぎたフレームのうち最新の 1 枚を描き、`MAX_PRESENTATION_LAG_US` を超えて遅れたフレームは捨てる」
  - `src/playout/buffer.rs` の `select` は、`MAX_PRESENTATION_LAG_US` 以内で表示時刻を過ぎた最も古いフレームを 1 枚描き、それより遅れたフレームを捨てる。最新の 1 枚を描くのではない
- `conceal` の相関窓の説明が実装と一致しない
  - SKILL: 「`conceal` は末尾の 2 周期分の相関から求めた周期で末尾を繰り返し、`conceal_us` 分を埋めた長さを返す」
  - `src/playout/stretch.rs` の `find_tail_period` は `TIME_STRETCH_TAIL_CORRELATION_LEN` (39 サンプル、4 kHz で 9.75 ms) の固定長窓で相関を測る。2 周期分ではない
- A/V 同期の実行条件の説明が実装と一致しない
  - SKILL: 「A/V 同期の制御も `observe` のたびに動く (音声と映像の両方に基準があり、基準の差が閾値の中にあるときだけ)」
  - `src/playout/timeline.rs` の `update_sync_delays` は、音声と映像の両方に基準があり `drifted_track()` が無いときに同期する。音声の TIMESTAMP が信用できない (`drifted_track() == Some(Track::Audio)`) ときは、映像だけを音声の到着基準へ合わせる (`update_sync_delays_to_arrival_audio`)。基準の差の閾値は `sharing_bases` の判定であり、同期の実行条件ではない
- モジュール表が公開モジュールを網羅していない
  - 表に `audio_clock` / `media_clock` / `pending_subgroup_buffer` / `video_decode_order` が無い
  - `playout` の `catch_up` / `feedback` / `publish_catch_up` / `splice_fade` / `timing` が無い
- `playout::timeline` の API 一覧が公開メソッドを網羅していない
  - 一覧に `observe_audio_playout` / `unshared_reason` / `delay_breakdown` / `playout_delay_us` / `PlayoutTimeline::audio_arrival_delay_us` と、free function の `audio_arrival_delay_us` が無い
  - `src/playout/timeline.rs` の `pub fn` と照合する

### examples の README / メッセージ

- `examples/moq-sub/README.md` が存在しない `--audio-datagram` を案内する
  - 「moq-pub は `--audio-datagram` でこの配送を選べる」
  - 実際は datagram 配送が既定であり、`--audio-subgroup` は subgroup stream への切り替えである (`examples/moq-pub/src/cli.rs` の `let audio_datagram = !audio_subgroup;`、`examples/moq-pub/README.md` の「既定は datagram」)
- `examples/moq-pub/src/error.rs` の datagram 上限超過メッセージが存在しない `--audio-datagram` を案内する
  - 「Disable --audio-datagram or increase --datagram-max-size」 (2 箇所。1 箇所はテストの期待値)
  - 正しい対処は `--audio-subgroup` で subgroup 配送に切り替えるか、`--datagram-max-size` を増やすことである
- `examples/moq-sub/src/pipeline.rs` の datagram 音声の doc コメントが存在しない `--audio-datagram` を案内する
  - 「moq-pub は `--audio-datagram` で 1 audio chunk = 1 Object = 1 Group の datagram を送る」
  - 実際は既定で datagram を送る
- `examples/moq-pub/README.md` の `--audio-subgroup` の説明に `--datagram-max-size` の上限説明が混ざっている
  - 「上限は `--datagram-max-size` で指定し、超える object はエラーになる」
  - `--audio-subgroup` は datagram を使わないため、この説明は当てはまらない。`--datagram-max-size` の行に同じ説明がある

### examples/moq-sub/src/pipeline.rs の doc コメント

- `run` の doc コメントが `video_subscribe_parameters` の直前に取り残されている
  - `/// パイプラインを実行する` から始まる `run` の説明が、`fn video_subscribe_parameters` の直前に付いている
  - `pub async fn run` には doc コメントが無い
  - `video_subscribe_parameters` の説明「映像トラックの SUBSCRIBE に載せるメッセージパラメータ」も同じコメントの末尾に混在している

### C4M トークンを載せる制御メッセージの列挙

- `README.md` の列挙に FETCH が無い
  - 「SETUP と各制御メッセージ (PUBLISH / SUBSCRIBE / REQUEST_UPDATE) の AUTHORIZATION_TOKEN として送信できます」
  - `examples/tokio-moq/src/moqt_client.rs` の `fetch` は `with_auth_parameters` で AUTHORIZATION_TOKEN を付与する。`src/message.rs` の `FETCH_ALLOWED_PARAMS` も `PARAM_AUTHORIZATION_TOKEN` を含む
- `examples/tokio-moq/README.md` の同じ列挙にも FETCH が無い
  - 「あわせて PUBLISH / SUBSCRIBE / REQUEST_UPDATE の AUTHORIZATION_TOKEN としてもすべてのトークンを送信する」
- `examples/tokio-moq/src/moqt_client.rs` の doc コメントにも FETCH が無い
  - `auth_tokens` フィールドの「PUBLISH / SUBSCRIBE / REQUEST_UPDATE には [`with_auth_parameters`] が...」
  - `establish_*` の「同じトークンを PUBLISH / SUBSCRIBE / REQUEST_UPDATE の AUTHORIZATION_TOKEN としても付与する」
  - テストの「残る送信経路 (PUBLISH / SUBSCRIBE / REQUEST_UPDATE)」

### README の相対リンク

- `README.md` は docs / examples / skills への相対リンクを持つ
  - `docs/moqt.md` / `docs/msf.md` / `docs/loc.md` / `docs/c4m.md`
  - `examples/moq-pub/README.md` / `examples/moq-sub/README.md` / `examples/tokio-moq/README.md`
  - `skills/shiguredo-moqt/SKILL.md`
- `Cargo.toml` の `include = ["/LICENSE", "/README.md", "/src/**"]` は docs / examples / skills を含まない
- crates.io は package の README を crate ページで描画するが、package 内の docs / examples / skills を URL として配信しない。`include` に追加しても README の相対リンクは解決しない
- docs.rs は rustdoc を表示するため README のリンクは対象外である
- 影響: crates.io の crate ページで README のリンクが壊れる

## 変更対象

- `docs/moqt.md`
- `skills/shiguredo-moqt/SKILL.md`
- `README.md`
- `examples/moq-sub/README.md`
- `examples/moq-pub/README.md`
- `examples/tokio-moq/README.md`
- `examples/moq-pub/src/error.rs`
- `examples/moq-sub/src/pipeline.rs`
- `examples/tokio-moq/src/moqt_client.rs`

## 対象外

- `docs/missing-features.md` の `--audio-datagram` と playout の制限の記述は 0323 が扱う。本 issue では変更しない
- `src/lib.rs` / `src/playout.rs` / `src/playout/scheduler.rs` の「時間圧縮・伸長」の記述は 0321 が扱う。本 issue では変更しない
- `src/` と `examples/tokio-moq/src/` の公開 API doc の欠落補完は 0321 が扱う。コメントの言語・表記は 0322 が扱う。`examples/tokio-moq/src/moqt_client.rs` は既存の C4M 列挙の誤りだけを修正し、`examples/moq-sub/src/pipeline.rs` の doc コメントの位置の誤りは本 issue で扱う
- コードの挙動 (パース、配送、playout の計算) は変えない
- `Cargo.toml` の `include` に docs / examples / skills を追加しない (README の絶対 URL 化で解決する)
- `CHANGES.md` は変更しない (CODEBASE.md の方針)

## 設計方針

- 実装の API と挙動から記述を再生成する。存在しないフラグ・古い API 名・実装と違う説明を削除または修正する
- `docs/moqt.md` の「音声の時間圧縮・伸長」を「音声の時間圧縮と欠落の補間」に直す。`src/playout/stretch.rs` は時間伸長を持たず、公開関数は `is_time_stretch_available` / `compress` / `concealment_end_gain` / `conceal` である
- SKILL.md の「時間伸縮」を「時間圧縮と欠落の補間」に直す (`description` / 特徴 / モジュール表)。「時間伸長は持たない」の記述は残す
- SKILL.md の playout の説明を実装から再生成する
  - `PlayoutBuffer::select` は「最新の 1 枚」ではなく、`MAX_PRESENTATION_LAG_US` 以内で表示時刻を過ぎた最も古いフレームを 1 枚描き、それより遅れたフレームを捨てる
  - `conceal` の相関窓は「末尾の 2 周期分」ではなく `TIME_STRETCH_TAIL_CORRELATION_LEN` (39 サンプル、4 kHz で 9.75 ms) の固定長
  - A/V 同期は、音声と映像の両方に基準があり `drifted_track()` が無いときに実行する。音声の TIMESTAMP が信用できないときは、映像だけを音声の到着基準へ合わせる (`update_sync_delays_to_arrival_audio`)。基準の差の閾値は `sharing_bases` の判定であり、同期の実行条件ではない
  - モジュール表に `audio_clock` / `media_clock` / `pending_subgroup_buffer` / `video_decode_order` と playout の `catch_up` / `feedback` / `publish_catch_up` / `splice_fade` / `timing` を追加する
  - timeline の API 一覧に `observe_audio_playout` / `unshared_reason` / `delay_breakdown` / `playout_delay_us` / `PlayoutTimeline::audio_arrival_delay_us` と free function の `audio_arrival_delay_us` を追加する
- `--audio-datagram` は存在しない。datagram 配送が既定であり、`--audio-subgroup` は subgroup stream への切り替えである。単純なフラグ名の置換をせず、意味が通る文に書き直す
  - `examples/moq-sub/README.md` は「moq-pub は音声を既定で datagram 配送し、`--audio-subgroup` で subgroup stream に切り替えられる」に直す
  - `examples/moq-pub/src/error.rs` は「Use --audio-subgroup or increase --datagram-max-size」に直す (2 箇所、テストの期待値も含む)
  - `examples/moq-sub/src/pipeline.rs` は「moq-pub は音声を既定で datagram 配送する (1 audio chunk = 1 Object = 1 Group)」に直す
- `examples/moq-pub/README.md` の `--audio-subgroup` の行から `--datagram-max-size` の上限説明を削除する
- `examples/moq-sub/src/pipeline.rs` の `run` の doc コメントを `pub async fn run` の直前に移し、`video_subscribe_parameters` の doc コメントだけをその直前に残す
- `README.md` と `examples/tokio-moq/README.md`、`examples/tokio-moq/src/moqt_client.rs` の C4M の列挙に FETCH を追加する
- `README.md` の docs / examples / skills へのリンクを GitHub の絶対 URL (`https://github.com/shiguredo/moqt-rs/blob/develop/...`) にする。`Cargo.toml` の `include` は変更しない
- コードの挙動は変えない。既存の API 名・フラグ名を削除・置換するときは、実装の意味を確認してから書き換える

## 完了条件

- `rg -n '時間圧縮・伸長' docs/moqt.md` が 0 件であること
- `rg -n '時間伸縮' skills/shiguredo-moqt/SKILL.md` が 0 件であること (「時間伸長は持たない」の記述は残る)
- SKILL.md の `PlayoutBuffer::select` / `conceal` / A/V 同期 / モジュール表 / timeline API の記述が `src/playout/buffer.rs` / `src/playout/stretch.rs` / `src/playout/timeline.rs` / `src/playout.rs` / `src/lib.rs` と一致していること
- `rg -n -- '--audio-datagram' examples/` が 0 件であること
- `examples/moq-sub/README.md` に datagram 配送が既定であることと `--audio-subgroup` の意味が書かれ、`examples/moq-sub/src/pipeline.rs` に datagram 配送が既定であることが書かれ、`examples/moq-pub/src/error.rs` が `--audio-subgroup` を対処として案内していること
- `examples/moq-pub/README.md` の `--audio-subgroup` の行に `--datagram-max-size` の上限説明が無いこと
- `run` の doc コメントが `pub async fn run` の直前にあり、`video_subscribe_parameters` の doc コメントがその直前に分離されていること
- `README.md` と `examples/tokio-moq/README.md` の C4M の列挙に FETCH があり、`examples/tokio-moq/src/moqt_client.rs` の C4M の列挙にも FETCH があること
- `README.md` の docs / examples / skills へのリンクが `https://github.com/shiguredo/moqt-rs/...` の絶対 URL であること (`rg -n '\]\((docs|examples|skills)/' README.md` が 0 件)
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること
- `markdownlint-cli2` が変更した Markdown で通ること

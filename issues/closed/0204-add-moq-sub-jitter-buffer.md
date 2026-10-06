# moq-sub の音声再生に jitter buffer を導入する

- Created: 2026-10-06
- Completed: 2026-10-06
- Branch: feature/add-moq-sub-jitter-buffer
- Polished: {YYYY-MM-DD}

## 目的

moq-sub は受信した音声 Object をデコードすると、到着した順にそのまま再生機器へ渡している。経路の揺らぎがそのまま音の途切れになるため、到着の遅れから目標遅延を学習する jitter buffer を入れる。

ライブラリ側には目標遅延の学習 (`src/playout/delay.rs` の `JitterDelayManager`) と、LOC の TIMESTAMP と受信側の壁時計の関係をトラックごとに学習して鳴らす時刻を返す時間軸 (`src/playout/timeline.rs` の `PlayoutTimeline`) が実装済みだが、example からは使われていない。

## 現状

- `examples/moq-sub/src/main.rs` の `run_raw_player` は、`audio_rx` から受け取った `DecodedAudioFrame` をその場で `raw_player::AudioPlayer::enqueue_audio` へ渡し、最初の 1 つで `AudioPlayer::play` を呼ぶ。到着した音は待たずに鳴る
- 到着の揺らぎを吸収するのは SDL のストリームバッファと raw_player のアプリキューの分だけである。揺らぎが 1 つの Object の長さ (Opus の 20 ms) を超えると音が途切れる
- 保持している音を捨てる基準も無い。再生ループが止まったあとに再開すると、溜まった音をそのまま鳴らして遅れが残る
- `DecodedAudioFrame` は `pts_us` (LOC の TIMESTAMP をマイクロ秒へ換算した値) を持つが、到着時刻は持たない。`examples/moq-sub/src/pipeline.rs` の `decode_audio_stream` は `audio_pts_us` で作った `pts_us` を詰めるだけである
- publisher の Timestamp は 0103 の完了まではメディア時刻 (Timescale 付き)、完了後は epoch マイクロ秒になる。どちらでも動く方式にする必要がある

## 設計方針

### 時間軸

- `src/playout/timeline.rs` の `PlayoutTimeline` を使い、音声トラックだけを観測する (`observe(Track::Audio, wall_clock_us, timestamp_us)`)
  - `present_us(Track::Audio, pts_us)` が鳴らす時刻 (受信側の壁時計、マイクロ秒) を返す。基準の遅れをトラックごとに学習して引き算するため、Timestamp がメディア時刻でも epoch マイクロ秒でも同じ式で扱える。0103 の完了を待たない
  - 目標遅延は `PlayoutTimeline::observe` の中で `JitterDelayManager` が学習する。観測が無いときの初期値は 80 ms である
  - `present_us` が `None` のとき (基準がまだ無い、基準を取り直した直後) は届いた順に鳴らす
  - `PlayoutTimeline::sync` を 1 秒ごとに呼び、目標遅延を学習値へ近づける。映像を観測しないため A/V 同期の制御は動かず、揺らぎが小さいときは目標遅延が毎秒 20 ms ずつ下がり、揺らぎが増えたときはすぐ上がる。これが無いと初期値の 80 ms から下がらない
- `targetLatency` (カタログ) の反映は 0104 の所掌とし、本 issue では扱わない
- 映像は本 issue の対象外とする

### 保持 (`examples/moq-sub/src/jitter_buffer.rs`)

`DecodedAudioFrame` と鳴らす時刻 (`play_at_us`) の組を保持する `AudioJitterBuffer` を追加する。時計もデバイスも触らず、現在時刻を引数で受ける純粋な構造体にして単体テストで検証する。

- `push(frame, play_at_us)` は `play_at_us` の昇順を保って挿入する。音声は 1 Object = 1 stream で並行に届くため、到着順と TIMESTAMP の順が入れ替わり得る
- `pop_releasable(now_us, lead_us)` は `play_at_us - lead_us` を過ぎた最も古い 1 件を返す。`lead_us` は再生機器のバッファが空にならないよう先行して積む分である
- `drop_late(now_us, max_lateness_us)` は `play_at_us + max_lateness_us` を過ぎた音を捨てる。再生ループが止まったあとに古い音を鳴らさないためである。閾値は `src/playout/scheduler.rs` の `AUDIO_PLAYOUT_MAX_LATENESS_US` を使い、example 側で独自の値を決めない
- 保持数の上限を超えたら最も古い音を捨てる。上限は `JitterDelayManager` が学習できる目標遅延の上限 (20 ms × 100 バケット = 2 秒) を超えない値にする
- 捨てた数 (遅れ / 上限超過) と保持数を数え、再生側がログに出せるようにする

### 再生 (`examples/moq-sub/src/main.rs` の `run_raw_player`)

- 音声フレームを受け取ったら `SystemTime::now()` をマイクロ秒で取り、`observe` してから `present_us` を `play_at_us` として `push` する
- ループの各回で `drop_late`、`pop_releasable` の順に処理し、取り出した音を `enqueue_audio` へ渡す。最初の 1 つで `play` する
- 音声チャネルが切断されたら保持している音をすべて吐き出す (末尾の音を捨てない)
- 目標遅延と保持数、捨てた数を一定間隔でログに出す
- `--audio-output-device none` では音声を出力しないため、保持も観測もしない (従来どおりデコード結果を数えるだけにする)
- `raw_player::AudioPlayer` を使う現行の経路を変えない。0104 が `VideoPlayer::enqueue_audio` へ載せ替えたあとも、この保持は積む前段としてそのまま使える

### 対象外

- 時間圧縮・伸長による追いつき (`src/playout/scheduler.rs` の `AudioPlayoutScheduler` と `src/playout/stretch.rs`) は扱わない。追いつきが必要になったら別 issue にする
- 映像の jitter buffer と A/V 同期は 0104 の所掌である。本 issue の完了後は、映像が到着後すぐ表示される一方で音声だけが目標遅延ぶん遅れて鳴る状態になる (目標遅延は初期値 80 ms から揺らぎに応じた値へ下がる)。0104 で映像側にも同じ時間軸を使わせるまで、このずれは残る

## 完了条件

- 音声が到着後すぐではなく、鳴らす時刻 (受信側の壁時計 + 学習した目標遅延) まで保持されてから再生されること
- 到着の揺らぎが増えると目標遅延が増えること。揺らぎが小さいときは初期値 (80 ms) から学習値へ下がること。学習した目標遅延と保持数がログで確認できること
- 保持している音が TIMESTAMP の昇順で再生されること (到着順の入れ替わりを含む)
- 再生ループが止まったあとに、閾値を超えて遅れた音が鳴らないこと
- `AudioJitterBuffer` の単体テストがあること (TIMESTAMP 順の挿入、`lead_us` の境界、遅れの破棄、上限超過、空のとき、揺らぎによる目標遅延の増加と減少、捨てた数の計上)
- 実機で 60 秒以上再生し、`raw_player::AudioPlayerStats::audio_buffer_ms` が 0 にならないこと (音が途切れないこと)。遅延が発散しないこと
- `--fake-capture-device` (20 ms 周期の一定到着) で目標遅延が発散せず、初期値の 80 ms から学習値 (20 ms 程度) へ下がって安定すること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

## 参照

- `src/playout/timeline.rs` の `PlayoutTimeline` / `TimelineConfig` / `Track`
- `src/playout/delay.rs` の `JitterDelayManager`
- `src/playout/scheduler.rs` の `AUDIO_PLAYOUT_MAX_LATENESS_US`
- `skills/shiguredo-moqt/SKILL.md` の `playout::timeline` / `playout::delay`
- draft-ietf-moq-loc-04 §2.3.1.1 (Timestamp)
- draft-ietf-moq-msf-01 §5.2.8 (Target latency)

## 解決方法

`examples/moq-sub/src/jitter_buffer.rs` に `AudioJitterBuffer` を追加した。
`playout::timeline` の `PlayoutTimeline` を持ち、`push` で `observe` と `sync` を行ってから
`present_us` を鳴らす時刻として保持する。保持は鳴らす時刻の昇順で、`pop_releasable` が
`lead_us` 手前まで来た最も早い音を返す。`drop_late` は `AUDIO_PLAYOUT_MAX_LATENESS_US` を
超えて遅れた音を捨て、保持数が `MAX_PENDING_FRAMES` (128 枚、学習上限の 2 秒相当) を
超えたら鳴らす時刻の早い音から捨てる。時計もデバイスも触らないため、単体テストで
順序・境界・破棄・計上を検証できる。

`examples/moq-sub/src/main.rs` の `run_raw_player` は、音声フレームの到着時に
`wall_clock_us` を渡して保持し、ループの各回で `drop_late` と `pop_releasable` を行ってから
`raw_player::AudioPlayer` へ積む。`AUDIO_OUTPUT_LEAD_US` (40 ms、Opus の 2 枚分) だけ
手前で積むため、SDL のストリームが空になりにくい。音声チャネルが切断されたら保持分を
すべて吐き出す。`--audio-output-device none` では保持も観測もしない。目標遅延・保持数・
再生機器のバッファ量・捨てた数を 5 秒ごとにログへ出す。

完了条件のうち、単体テスト (8 件) と `make test` / `make clippy` / `make fmt` は通している。
実機での 60 秒再生と目標遅延の収束は relay と音声出力が必要なため未実施であり、
次の手順で確認する。

```console
cargo run -p moq-pub -- --url moqt://<relay> --fake-capture-device
cargo run -p moq-sub -- --url moqt://<relay>
```

`Audio jitter buffer: target_delay=...ms pending=... player_buffer=...ms released=...` の
`target_delay` が 80 ms から学習値へ下がって安定し、`player_buffer` が 0 にならないことを
確認する。映像は本 issue の対象外であるため、0104 で映像側にも同じ時間軸を使わせるまでは
音声だけが目標遅延ぶん遅れる。

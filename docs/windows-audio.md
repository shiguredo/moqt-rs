# Windows の moq-pub で音声のピッチが下がり定期的に途切れる問題の調査

- 調査日: 2026-10-19
- 対象: moqt-rs develop (f297e07)、moqt-js (8c7d3d21)、shiguredo_audio_device 2026.4.0-canary.1
- 症状: Windows の moq-pub から送信した実マイク音声を moqt-devtools で受信・再生すると、定期的に途切れ、音のピッチが下がる。macOS の moq-pub では問題ない。

## 1. 結論

原因は 2 つある。主原因はタイムスタンプではなく、Windows の音声キャプチャのフォーマット不一致である。

1. 主原因: Windows の WASAPI キャプチャは `AudioCaptureConfig` の `sample_rate` / `channels` を無視し、共有モードの mix format (デバイスの Default Format のチャンネル数とサンプルレート) をそのまま返す。moq-pub は実測値を検証せず 48 kHz 1ch 前提で Opus 符号化・カタログ・OpusHead を作るため、実フォーマットが 48 kHz 1ch 以外だとピッチと実時間がずれる。2ch 48 kHz または 96 kHz 1ch なら 1 オクターブ下がる。
2. 副原因: Windows の `timestamp_us` は `GetBuffer` 直後の `QueryPerformanceCounter` (読み出し時刻) であり、デバイスが記録した先頭フレームの時刻 (`pu64QPCPosition`) ではない。読み出しの burst で Timestamp が圧縮され、moq-pub の `AudioCaptureTimeline` がフレームごとに基準を取り直すため、Timestamp の揺れ・同一時刻化・逆行が起きる。これは A/V 同期と再生タイミングの揺らぎを作るが、ピッチは変えない。
3. 受信側 (moqt-devtools) にはピッチを変える処理が無い。`AudioBufferSourceNode.start(when)` だけで再生し、時間圧縮は NetEq accelerate の移植でピッチを保つ。したがってピッチ低下は配信側の PCM の解釈に由来する。
4. 「定期的に途切れる」は、1 つの Opus フレームの実音声長 (D) が LOC Timestamp の刻み (T) より長くなること (D > T) で起きる。moqt-devtools が到着基準で再生する経路 (音声だけ購読、または jitter buffer 無効) では backlog drop がほぼ 1 フレームおきに発生する。映像と音声を同時に購読する時間軸経路では drop せず、再生の遅れ (lateness) が増え続ける。

## 2. 根拠: Windows のキャプチャは要求フォーマットを守らない

### 2.1 shiguredo_audio_device 2026.4.0-canary.1 の WASAPI 実装

- `src/capture_wasapi.rs:50` で `config` から使うのは `device_id` だけである。`sample_rate` と `channels` は保存されるが (`:117`)、それ以外で参照されない。
- `src/capture_wasapi.rs:58-79` で `GetMixFormat()` の値をそのまま共有モードの `Initialize` に渡す。要求フォーマットを構築する処理も `AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM` も無い。
- 返る `frame.channels` / `frame.sample_rate` は mix format の値である (`src/capture_wasapi.rs:67-68`, `:311-318`)。`AudioCapture::sample_rate()` / `channels()` も同じ実測値を返す (`src/capture.rs:149-167`)。
- つまり `AudioCaptureConfig` は Windows では「要求」ではなく「無視されるヒント」であり、API の見た目と実装が一致していない。

### 2.2 プラットフォームごとの差

- macOS: `AudioQueueNewInput` に要求フォーマット (48 kHz / 1ch / S16) を構築して渡す (`src/audio_coreaudio.m:457-472`)。変換は AudioQueue が行うため、返るフレームは要求どおりになる。timestamp も `AudioTimeStamp.mHostTime` (バッファ先頭のキャプチャ時刻) である (`src/audio_coreaudio.m:28-58`)。
- Linux: PulseAudio は要求 `pa_sample_spec` (`src/audio_pulse.c:511-518`)、PipeWire は要求 `spa_audio_info_raw` (`src/audio_pipewire.c:519-523`) を指定し、サーバ側で変換される。
- Windows: mix format 固定であり、要求フォーマットが実質効かないのは WASAPI だけである。

### 2.3 Windows の mix format としてあり得る値

- `GetMixFormat` は共有モードのエンジン内部処理形式を返し、アプリのストリームは通常デバイスと同じチャンネル数・同じサンプルレートでなければならない ([Device Formats](https://learn.microsoft.com/en-us/windows/win32/coreaudio/device-formats))。
- ユーザーが mmsys.cpl の Default Format で選んだ形式が共有モードのデバイス形式になり、エンジンは同じチャンネル数・レートのままサンプルを float 化する。マイクの Default Format は 1ch / 2ch、44100 / 48000 / 96000 / 192000 のいずれもあり得る。2ch 48000 Hz は Windows で一般的である。
- 24 bit などを Default Format で選んでも mix format は float32 になるのが通常だが、float を明示的に保証する一次資料は無い。

## 3. 根拠: moq-pub は 48 kHz 1ch 固定

- `examples/moq-pub/src/audio_capture.rs:28-32` は `AudioCaptureConfig { sample_rate: 48_000, channels: 1 }` を渡すだけで、`AudioCapture::sample_rate()` / `channels()` を呼ぶ箇所は moq-pub に無い。`audio_capture.rs:41` は実測値ではなく固定文字列 "48000 Hz, 1ch" をログする。
- `examples/moq-pub/src/pipeline.rs:133-140` の `AUDIO_SAMPLE_RATE` / `AUDIO_CHANNELS` / `AUDIO_CHANNEL_CONFIG` が 48 kHz / 1ch 固定である。`pipeline.rs:607-613` の Opus エンコーダ、`pipeline.rs:703-710` のカタログ、`pipeline.rs:635-638` の OpusHead がすべてこの定数を使う。
- `examples/moq-pub/src/pipeline.rs:2969-2984` の `extract_pcm_i16` は `frame.as_s16()` / `as_f32()` の生スライスをそのまま返す。`frame.channels` / `frame.sample_rate` / `frame.frames` を見ないため、ステレオのインターリーブをモノラルとして扱う。
- encoder/opus.rs:26-32 は 48 kHz / 1ch / 20 ms 固定であり、`samples_per_frame()` は 960 である (`encoder/opus.rs:53-56`)。`encoder/opus.rs:77-87` の OpusHead も 48 kHz / 1ch を書く。
- 配信側の追いつき (`AudioPublishCatchUp`) は `read_wall_clock_us - timestamp_us` で判定する。timestamp が読み出し時刻由来で lag がほぼ 0 になるため、この問題では発動しない。

## 4. 根拠: 受信側 (moqt-devtools) はピッチを変えない

- `playbackRate` / `detune` / `preservesPitch` は moqt-js のリポジトリ全体で 0 件である。音声の予約は `AudioBufferSourceNode.start(whenSeconds)` だけ (`src/playout/audioPlayoutSession.ts:597-613`)。
- AudioBuffer は復号結果の `numberOfChannels` / `numberOfFrames` / `sampleRate` をそのまま使う (`src/playout/audioPlayoutSession.ts:257-260`, `:605`)。Web Audio がバッファのサンプルレートを AudioContext のレートへ変換するのは等価な波形変換であり、速度・ピッチを変える指定ではない。
- 時間方向の加工は 2 つだけである。時間圧縮 (`src/playout/audioTimeStretch.ts:281-328`) はピッチ周期 1 つ分を削る NetEq accelerate の移植で、ピッチを保つ。補間 (`audioTimeStretch.ts:411` 以降) は末尾の周期を繰り返す。どちらもピッチを変えない。
- moqt-devtools はライブラリと同じ `AudioPlayoutSession` を使う (`devtools/src/hooks/useSubscriber.ts:77`, `:1241`)。

## 5. 症状との対応

### 5.1 ピッチ

960 サンプル (Opus 1 フレーム = 20 ms と主張する量) を、実サンプルレート R、実チャンネル数 C のインターリーブ列から切り出した場合を考える。

- 実音声長: `960 / (R * C)` 秒
- 再生されるピッチ比: `48000 / (R * C)`
- 半音: `12 * log2(48000 / (R * C))`

| mix format | 1 Object の実音声長 | ピッチ比 | 半音 |
| --- | --- | --- | --- |
| 48 kHz 1ch | 20.000 ms | 1.000 | 0.00 |
| 48 kHz 2ch | 10.000 ms | 0.500 | -12.00 (1 オクターブ下) |
| 96 kHz 1ch | 10.000 ms | 0.500 | -12.00 (1 オクターブ下) |
| 96 kHz 2ch | 5.000 ms | 0.250 | -24.00 (2 オクターブ下) |
| 44.1 kHz 1ch | 21.769 ms | 1.088 | +1.47 (上がる) |
| 44.1 kHz 2ch | 10.884 ms | 0.544 | -10.53 (下がる) |

生成した 440 Hz の波形で確認した例:

- 48 kHz 2ch (L=R) のインターリーブ列を 48 kHz mono として再生: 220.2 Hz (-12.0 半音)
- 96 kHz mono の列を 48 kHz mono として再生: 220.2 Hz (-12.0 半音)
- 44.1 kHz mono の列を 48 kHz mono として再生: 480.0 Hz (+1.5 半音)

「ピッチが下がる」という症状は、チャンネル数が 2 以上、またはサンプルレートが 48 kHz より大きいことを意味する。44.1 kHz 1ch なら逆にピッチは上がるため、症状と一致しない。

### 5.2 定期的な途切れ

48 kHz 2ch のキャプチャでは、10 ms のパケット (480 frames × 2ch = 960 interleaved) がちょうど 1 Opus フレームになる。配信される Object は 10 ms 間隔で、復号後の長さは 20 ms である。つまり D = 20 ms、T = 10 ms となり、音声の内容が Timestamp の刻みより 2 倍長い。

moqt-devtools の再生経路によって挙動が変わる。

- 到着基準の経路 (音声だけ購読、または jitter buffer 無効、または TIMESTAMP に TIMESCALE がある): `src/playout/audioPlayout.ts:587-628` の `scheduleByArrival` が前の音の終端に並べる。
  20 ms の音を 10 ms 間隔で並べることはできないため、バッファが溜まって backlog drop になる (`audioPlayout.ts:103-107`, `:609-619`)。
  参考値 (20 ms の音を 10 ms 間隔で 30 秒) では 1513 フレーム再生 / 1487 フレーム破棄となり、交互に途切れる。
- 時間軸経路 (映像と音声を購読、jitter buffer 有効): 前の音の終端より前に置かず (`audioPlayout.ts:432-434`)、時間圧縮の上限は `min(lateness, durationSeconds/2, queueSlack)` (`:478-482`)。20 ms フレームで 1 回に削れるのはピッチ周期 1 つ (周期探索の上限から最大 7.25 ms) のため、10 ms の超過を削りきれず、lateness が毎フレーム 2.75 ms 以上ずつ増える。drop はせず、遅延の増大と圧縮による粗さになる。
- TIMESTAMP の段差が 2 秒以上になると `src/playout/timeline.ts:121` の `PLAYOUT_DISCONTINUITY_MS` で時間軸が reset され、音声は到着基準へフォールバックする。読み出し時刻由来の揺れがここに達する場合は、reset のたびに隙間が出る。

### 5.3 moqt-devtools で確認できる統計

Copy for LLM の JSON で次の値を見ると、どの経路かを切り分けられる。

- 到着基準 + D > T: `audio.playoutTiming.slackMs` / `latenessMs` が "-" (null)、`arrivalPlannedFrames` が `playedFrames` と同程度、`audio.playoutTiming.missedByReason.backlog` と `audio.playoutDrops` が増え、`audio.playoutCompressedMs` は 0。
- 時間軸 + D > T: `slackMs` / `latenessMs` があり、`latenessMs` / `startDelayMs` / `lastStartMs` が単調に増え、`audio.playoutCompressedMs` が増える。`audio.playoutDrops` は 0。
- TIMESTAMP の時刻ずれ: `audio.receiveDelayMs` / `audio.maxReceiveDelayMs` が増える。`stats.avSync.unsharedReason` が "none" 以外になり、`baseDifferenceMs` が大きくなる。

## 6. 実機での確認手順

1. Windows のマイクの Default Format を確認する (設定 > システム > サウンド > 録音 > マイクのプロパティ > 詳細)。1ch 48000 Hz 以外なら、この問題の条件を満たす。
2. moq-pub を `--fake-capture-device` で実行する。疑似キャプチャは 48 kHz / 1ch / S16 を生成するため、実マイク経路の問題かを切り分けられる。疑似キャプチャで症状が消えれば、キャプチャのフォーマット不一致が原因である。
3. 実測フォーマットをログに出す。未実装だが、`audio_capture.rs` で `capture.sample_rate()` / `capture.channels()` を起動時にログするのが最短である。audio-device-rs の `device_info` example (`cargo run --example device_info`) でもデバイスの mix format を確認できる。
4. moqt-devtools の Copy for LLM で 5.3 の統計を確認する。

## 7. 修正案

### 7.1 根本修正: shiguredo_audio_device の WASAPI キャプチャ

- 要求フォーマット (48 kHz / 1ch / S16) の `WAVEFORMATEX` を構築し、`Initialize` に `AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY` を渡す。`windows` 0.62.2 に定数がある (`Audio/mod.rs:1494`, `:1500`)。NAudio の WasapiCapture も共有モードのキャプチャで同じフラグを使っている。
- 共有モード + イベント駆動では `hnsBufferDuration` と `hnsPeriodicity` の両方を 0 にする必要がある ([IAudioClient::Initialize](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize) の Remarks)。現状は 10 ms (100_000) を渡している。
- `AUTOCONVERTPCM` が使えない場合は mix format へフォールバックするか、明示的なエラーを返す。黙って mix format を返す現状の挙動は、呼び出し側が要求どおりと誤解するため危険である。
- timestamp は `GetBuffer` の `pu64QPCPosition` (100 ns 単位) を 10 で割って使う ([IAudioCaptureClient::GetBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer))。
  `AUDCLNT_BUFFERFLAGS_TIMESTAMP_ERROR` が立つ場合は `pu64DevicePosition` とサンプルレートから補間するか、読み出し時刻へフォールバックする。
- `AUDCLNT_BUFFERFLAGS_SILENT` のパケットをスキップせず、ゼロ PCM として渡す。スキップするとタイムラインに穴ができ、受信側は直前の音で補間する。
- `determine_audio_format` は `WAVE_FORMAT_EXTENSIBLE` の `cbSize` と `SubFormat` を検証し、24 bit / 32 bit PCM を S16 と誤判定しないようにする。現状は PCM の 24/32 bit container を S16 と扱い、`data_size = frames * channels * 2` でバッファの先頭だけを読むため波形が壊れる (`src/capture_wasapi.rs:288-301`, `src/device_wasapi.rs:220-239`)。
- 変更後は canary.2 としてリリースし、moq-pub の依存を更新する。

### 7.2 暫定・防御: moq-pub

- `audio_capture.rs` で `AudioCapture::new` の直後に `capture.sample_rate()` / `channels()` を読み、実測値をログする。48 kHz / 1ch 以外なら起動時に明確なエラー (または警告) にする。純粋な検証関数に切り出せば macOS の unit test で検証できる。
- `extract_pcm_i16` を `frame.channels` 対応にする。2ch は (L + R) / 2 でモノラルへ downmix する。これだけで 2ch 48 kHz のピッチと実時間の過剰は直る (3ch 以上はエラー)。ただし 96 kHz などのレート差は直らないため、7.1 の根本修正と併用する。
- `AudioCaptureTimeline` をサンプル数基準にする。`observe_frame` がフレームごとに `head` を取り直す (`pipeline.rs:2874-2878`) ため、読み出し時刻のジッタで Timestamp が揺れる。前回の Timestamp + 消費サンプル数で進め、フレームの実測時刻との差が閾値を超えたときだけ再同期する。出力 Timestamp は前回以上に clamp する。
- `samples_to_micros` の計算に `frame.frames` / `frame.sample_rate` / `frame.channels` を使う。現状の `buffered_samples` は 48 kHz モノラル換算であり、2ch では実時間の半分として扱われる。

### 7.3 検証

- Windows の CI はコンパイル確認のみで、実デバイスの実行をしない (`.github/workflows/ci-windows.yml`)。実マイクを使う手動 E2E の手順を用意する。
- 修正後は `--fake-capture-device` と実マイクの両方で、moqt-devtools の 5.3 の統計が正常値に戻ることを確認する。

## 8. 未確定点

- 実機の mix format が 2ch 48 kHz なのか 96 kHz 1ch なのかは実測ログが無いため不明である。どちらも「1 オクターブ下」の症状と一致する。
- 途切れが到着基準経路の backlog drop なのか、時間軸経路の遅延増大なのかは、moqt-devtools の統計で確定できる。
- `AUTOCONVERTPCM` 併用時の追加レイテンシと CPU 負荷は一次資料に記載が無く不明である。
- 無音パケットのスキップが実際にどの程度起きるかは実測が必要である。

## 9. 参照

- [IAudioClient::GetMixFormat](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-getmixformat)
- [Device Formats](https://learn.microsoft.com/en-us/windows/win32/coreaudio/device-formats)
- [IAudioCaptureClient::GetBuffer](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudiocaptureclient-getbuffer)
- [IAudioClient::Initialize](https://learn.microsoft.com/en-us/windows/win32/api/audioclient/nf-audioclient-iaudioclient-initialize)
- [AUDCLNT_STREAMFLAGS_XXX Constants](https://learn.microsoft.com/en-us/windows/win32/coreaudio/audclnt-streamflags-xxx-constants)

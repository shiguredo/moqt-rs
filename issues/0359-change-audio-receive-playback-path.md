# 音声の受信から再生開始までの経路を見直す

- Created: 2026-10-20
- Completed: 2026-10-20
- Branch: develop
- Polished: {YYYY-MM-DD}
- Reporter: @voluntas

## 目的

qmux 経由の音声がプチプチする。QMux の DATAGRAM は TCP 1 本の順序付きストリームに載るため、TCP の再送待ちで音声がまとめて遅れて届く。現状の example は、復号済みフレームを `AudioJitterBuffer` で目標の 40 ms 前まで保持し、最初の 1 フレームを積んだ直後に `play()` するため、先行バッファが蓄積されず、リリースの遅れがそのまま SDL のアンダーランになる。受信から再生開始までの経路を見直し、復号した音をその場で積んで再生開始を目標時刻に合わせる。

## 現状

- `examples/moq-sub/src/main.rs` の `AudioJitterBuffer` は `play_at_us` を push 時に固定し、`pop_releasable` で `play_at_us - AUDIO_OUTPUT_LEAD_US` を過ぎた音を返す。目標変更や世代の取り直しを反映しない。
- 同じファイルの `enqueue_pcm` は最初の 1 フレームを積んだ直後に `play()` する。そのため先行量は定常状態で 1 フレーム程度にしかならず、`AUDIO_OUTPUT_LEAD_US` は安全余裕として働かない。
- 音声と映像を同じスレッドで回し、`VideoPlayer::poll_events()` が vsync 有効の `SDL_RenderPresent` を呼ぶため、表示周期の間だけ音声の積み込みが止まる。
- 到着時刻を player スレッドの mpsc 取り出し時に測っている。復号直前の到着は catch-up 用に測って捨てている。`DecodedAudioFrame` に到着時刻と復号完了時刻が無い。
- datagram 受信タスクはバッチ処理のたびに 10 ms sleep する。メインループは 1 回に 1 件しか `try_recv` しない。

## 設計方針

- `AudioJitterBuffer` を削除し、復号済みの音声は受信したその場で時間軸へ観測してスケジューラへ渡し、PCM を積む。再生開始は最初の `start_at_us` に達してから行い、それまでのフレームを先行バッファとして蓄積する。
- `raw_player::VideoPlayer` の vsync を無効にして、音声の積み込みが表示待ちで止まらないようにする。
- `DecodedAudioFrame` に受信の到着時刻と復号の完了時刻を足す。時間軸への観測には復号の完了時刻を使い、player スレッドの揺れを遅延学習へ入れない。
- datagram 受信タスクの sleep は空バッチのときだけにする。`recv_datagrams` は 1 件以上届くまで待つため、qmux と quic では sleep しない。
- メインループは `audio_rx` を毎回すべて drain する。
- 出力が枯れたら `AudioPlayoutScheduler::note_underrun` で基準を戻し、計器へ記録する。

## 完了条件

- 再生開始が最初の目標時刻に行われ、開始時の出力バッファが目標遅延ぶん蓄積されることを単体テストで固定する。
- vsync を無効にした状態で映像と音声を同時に再生できること。
- 到着時刻と復号完了時刻が `DecodedAudioFrame` に載り、時間軸が復号完了時刻で観測されること。
- 空バッチ以外で datagram 受信タスクが sleep しないこと。
- 5 秒ログに出力バッファの最小値とアンダーランの回数・合計が出ること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 参照

- `examples/moq-sub/src/main.rs` の `run_raw_player` / `AudioJitterBuffer` / `enqueue_pcm` / `play_decoded_audio`
- `examples/moq-sub/src/jitter_buffer.rs`
- `examples/moq-sub/src/pipeline.rs` の datagram 受信タスク
- `examples/tokio-moq/src/transport.rs` の `recv_datagrams`
- `raw_player` の `AudioPlayer` / `VideoPlayer`

## 解決方法

- `AudioJitterBuffer` を削除し、復号した音声を受信したその場で時間軸へ観測して出力のキューへ積むようにした。再生開始は最初の音の `start_at_us` に達してから行い、それまでのフレームを先行バッファとして蓄える。
- `DecodedAudioFrame` に復号直前の到着時刻 `arrival_us` と復号完了時刻 `decoded_us` を追加した。時間軸への観測には `decoded_us` を使い、プレイヤースレッドの揺れを遅延学習へ入れない。
- datagram 受信タスクの sleep を空バッチのときだけにし、メインループは音声を毎回全件取り出すようにした。映像の vsync を無効にし、同じスレッドの表示待ちで音声供給が止まらないようにした。
- 出力が枯れたら `note_underrun` で並べる基準を戻し、枯れが明けた長さを計器へ記録する。
- 単体テストを追加し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ることを確認した。

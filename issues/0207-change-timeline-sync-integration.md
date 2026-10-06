# A/V 同期の遅延制御を時間軸へ統合する

- Created: 2026-10-06
- Completed: {YYYY-MM-DD}
- Branch: feature/complete-playout
- Polished: {YYYY-MM-DD}

## 目的

いまの A/V 同期の制御は、直近の観測から求めた経路の相対遅延を制御量にしている。観測のたびに動く揺らぎがそのまま制御量に入るため、表示時刻の差を合わせられない。表示の遅れの差そのもので合わせる方式へ変え、制御を時間軸の中に持つ。

## 現状

- `src/playout/sync.rs` の `StreamSynchronization::compute_delays` が、`compute_relative_delay` が直近の観測から求めた相対遅延 (`SyncMeasurement`) を平滑化し、不感帯を置いて片側だけを動かす
- `src/playout/timeline.rs` の `sync` がこれを 1 秒ごとに呼び、返った遅延の下限を `TrackState::presentation_delay_us` にそのまま入れている。`delay_us` はこの値を上限で切ったものである
- 同期が動かなかったときは `decay_presentation_delays` が両トラックの `presentation_delay_us` を 1 秒あたり `TIMELINE_DELAY_DECAY_US_PER_SECOND` だけ下げる
- `SyncDelays` / `StreamSynchronization` / `SyncMeasurement` / `compute_relative_delay` は公開 API だが、利用しているのは `timeline` の中だけである

## 設計方針

制御量を「下限を外した表示の遅れ」の差そのものにし、観測のたびに適用する。

- トラックごとに同期が足した遅延 (`sync_extra_us`) を持ち、表示の遅れ = 自分の揺らぎから求めた値 (`targetLatency` との大きい方) + 足した分 とする。`presentation_delay_us` はこの和を上限で切った値になる
- ずれが `SYNC_MIN_DELTA_MS` を超えたら、先行する側へ足して「後行側 - 不感帯」に合わせる。足すのは即座に行う。遅らせる向きの変更は、既に積んだフレームの表示時刻を未来へ動かすだけで並べ替えは起きない
- 足した分は、下限を外した表示の遅れから決まる目標へ毎秒 `TIMELINE_DELAY_DECAY_US_PER_SECOND` までで戻す。両側を同じ速さで戻すため、戻している間もずれは開かない。自分の下限が同時に下がっているときは、その分だけ戻す量を減らす。観測の間隔で按分するため、観測が疎でも速さは変わらない
- 戻したあとにもう一度そろえる (片側だけ戻すと、その分だけずれが開く)
- 音声と映像の両方を観測していて、基準の差が閾値の中にあるときだけ行う。TIMESTAMP が壁時計からずれているトラックがあると、ずれが単調に増えてもう片方の表示が未来へ伸びるためである
- 1 秒間隔の制御をやめる。`observe` から呼び、間隔は `now_us` の差で按分する
- `src/playout/sync.rs` とその公開 API (`SyncDelays` / `StreamSynchronization` / `SyncMeasurement` / `compute_relative_delay`)、`PlayoutTimeline::sync`、`TIMELINE_SYNC_INTERVAL_US` / `TIMELINE_SYNC_TOLERANCE_US` を削除する (後方互換のない変更)
- `examples/moq-sub/src/jitter_buffer.rs` が `sync` を呼ばないようにする (観測が制御を含むため)

## 完了条件

- ずれが不感帯に収まること。観測のたびに適用され、1 秒ごとの制御を待たないこと
- 足した分が毎秒 `TIMELINE_DELAY_DECAY_US_PER_SECOND` までで戻ること。両側を同じ速さで戻すこと
- 自分の下限が下がった分だけ戻す量が減ること
- 片方のトラックしか観測していないときは制御が動かないこと
- `timeline` の単体テストと PBT が更新されていること。`src/playout/sync.rs` とその公開 API が残っていないこと
- `examples/moq-sub` が `make test` と `make clippy` を通ること
- `make test` (`cargo test --workspace`) と `make pbt` と `make clippy` と `make fmt` が通ること

## 参照

- `src/playout/timeline.rs` の `PlayoutTimeline` / `TrackState`
- draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.11 (Render group)

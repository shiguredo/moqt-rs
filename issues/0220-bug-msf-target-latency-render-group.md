# 別の render group の targetLatency が購読中の track に適用される

- Created: 2026-10-09
- Completed: 2026-10-13
- Branch: feature/fix-msf-target-latency-render-group
- Polished: 2026-10-13
- Updated: 2026-10-11

## 目的

draft-ietf-moq-msf-01 §5.2.8 (Target latency) は、同じ render group に属する track は同一の
`targetLatency` を持たなければならない (MUST) と定める。render group は一緒に表示される track の
集合であり、購読する track の表示の遅れはその group が宣言した値で決まるべきである。

`examples/moq-sub/src/pipeline.rs` の `catalog_target_latency_ms` は `catalog.tracks` の全 track の
最大値を採るため、購読する track とは別の render group がより大きい値を宣言していると、その値が
購読中の track の表示の遅れの下限と音声目標遅延の閉ループの上限に適用される。たとえば group 1 が
500 ms、group 2 が 100 ms を宣言するカタログで group 2 の track を購読すると、100 ms で表示できる
はずの track に 500 ms が掛かる。表示の遅れは `TIMELINE_MAX_PRESENTATION_DELAY_MS` (500 ms)、音声の
閉ループの上限は `AUDIO_DELAY_FEEDBACK_MAX_US` (300 ms) で頭打ちになるため、観測される差はその
範囲に収まるが、購読する group の宣言値が無視される点は変わらない。§5.2.8 が group 単位で同一性を
求める設計意図に反する。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `catalog_target_latency_ms` は `MsfTrack::target_latency`
  を持つ track を集めて最大値を返す。`render_group` は参照しない。
- 返した値は同じファイルの `run` が `target_latency_ms` へ格納し、`src/playout/timeline.rs` の
  `set_target_latency_ms` が (a) `natural_delay_us` による表示の遅れの下限 (A/V 同期の基準)、
  (b) `set_ceiling_us` による音声目標遅延の閉ループの上限、の両方に使う。1 つの値が購読する
  すべての track に同じように掛かる。
- 購読する track は `receive_catalog` が codec で選ぶ (映像は av01 / avc1 / hvc1 / hev1、音声は
  opus)。`render_group` は見ていない。
- `MsfTrack::render_group` は `Option<u64>` で、宣言が無ければ `None` である。library の検証
  (`validate_group_target_latency`) は同じ group の track 同士の同一性だけを見るため、
  group ごとに異なる値を宣言したカタログは正当に通り、本 issue の経路に到達する。
- §5.2.8 の「If isLive is FALSE, this field MUST be ignored.」は、moq-sub が受け取るカタログが必ず
  通る `MsfCatalogDocument::decode` の正規化 (`decode_track` / `decode_clone_track` /
  `MsfCloneTrack::into_track`) で満たされている。到達可能な経路で `target_latency` を持つ
  `is_live == false` の track は現れないため、example 側で `is_live` を見る必要はない
  (`tests/test_msf/track_fields.rs` の `target_latency_ignored_when_is_live_false` で固定済み)。
- 変更対象の判定に必要な既存テストは `examples/moq-sub/src/pipeline.rs` の
  `#[cfg(test)] mod tests` に 5 件ある (`catalog_target_latency_reads_the_value` /
  `catalog_target_latency_ignores_tracks_without_the_value` /
  `catalog_target_latency_is_none_without_the_value` /
  `catalog_target_latency_takes_the_larger_value` /
  `catalog_target_latency_ignores_values_out_of_range`)。ヘルパ `track_with_target_latency` は
  `render_group` を設定しないため、引数が変わると 5 件とも更新が要る。

## 設計方針

- 「購読する track」は実際に SUBSCRIBE する track を指す。`receive_catalog` は codec で選ぶだけで
  `--no-video` / `--no-audio` を見ないため、`receive_catalog` は codec で選んだ映像 / 音声の
  `render_group` を `CatalogResolution` に含めて返し、`run` が `subscribe_video` /
  `subscribe_audio` を決めた後に `catalog_target_latency_ms` を呼ぶ。
- `catalog_target_latency_ms` の対象を、購読する track の render group に属する track に限る。
  購読する track ごとに、`render_group` が `Some(g)` なら group `g` に属する track、`None` なら
  その track 自身を対象に含める (既定では 2 本購読するため単数では足りない)。
  - 購読する track がすべて `render_group` を宣言していないとき: 現行どおり `tracks` の全 track の
    最大値を採る (group が分からない以上、安全側に倒し、現行の挙動を変えない)。
  - いずれかが宣言しているとき: 対象に含めた track の `target_latency` の最大値を採る。映像と音声が
    別の group を宣言している場合も、時間軸が持てる値は 1 つだけであるため両方の group の最大値を
    両方の track に掛ける (§5.2.8 が group 内の同一性を MUST とするため、複数あるときは安全側に
    倒す現行の規則を group 内で維持する。片方だけが宣言している場合も、宣言していない側の
    購読 track 自身の値は対象に残る)。
  - 対象に含めた track が 1 つも `target_latency` を持たないとき: `None` を返す (現行どおり)。
- 映像と音声で別々の値を使うには `PlayoutTimeline` の API 変更が要る。§5.2.8 は group 内の同一性
  だけを求めるため別 group の値が異なるカタログは正当であり、これは残る限界として記録する
  (track ごとの値の適用は本 issue の範囲外)。
- `is_live` による絞り込みは行わない (前掲のとおり decode 層で満たされている)。library 側の
  正規化・検証は変えない。
- `targetLatency` を持つ track が 1 つも無いときの現行挙動 (`None` を返す。0230 以降、時間軸は
  音声の目標遅延を閉ループの観測から決め、カタログに値があるときだけ閉ループの上限として渡す) は
  変えない。
- `catalog_target_latency_ms` の doc コメントに、group 単位で選ぶ理由と §5.2.8 の MUST を追記する。
- 完了条件のテストは `examples/moq-sub/src/pipeline.rs` の `#[cfg(test)] mod tests` に置く
  (`catalog_target_latency_ms` は private なため)。

## 完了条件

- 購読する track の render group が、別の group が宣言するより小さい `targetLatency` を宣言して
  いるとき、購読する group の値だけが採られることのテスト。
- 購読する track が複数の render group にまたがるとき、それらの group の値の最大値が採られること
  のテスト。
- 購読する track の片方だけが `render_group` を宣言しているとき、宣言していない側の track 自身の
  `targetLatency` も対象に残ることのテスト。
- 購読する track が `render_group` を宣言しているとき、実際に購読する track の group だけで対象が
  決まること (`--no-audio` のように購読しない track の group を含めないこと) のテスト (購読する
  track が 1 つも宣言していないときは全 track が対象になるため、この限定が要る)。
- 購読する track が 1 つも `render_group` を宣言していないとき、従来どおり `tracks` の全 track の
  最大値が採られることのテスト。
- 従来の挙動 (値のある track から最大値を採る / 値が無ければ `None`) が既存テストで維持されている
  こと。最大値の規則は `catalog_target_latency_takes_the_larger_value` と
  `catalog_target_latency_ignores_tracks_without_the_value`、`None` は
  `catalog_target_latency_is_none_without_the_value` が固定しており、新規テストを重複して足さない。
  引数が変わるため既存 5 件を新しい引数に合わせて更新する。
- ソースコードのコメントに issue 番号や issue への言及を書かないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること

## 解決方法

- `examples/moq-sub/src/pipeline.rs` の `catalog_target_latency_ms` の対象を、実際に購読する track の
  `renderGroup` に属する track に限った。カタログ全体の最大値を採っていたため、購読する track とは別の
  render group がより大きい `targetLatency` を宣言していると、その値が購読中の track の表示の遅れの
  下限と音声目標遅延の閉ループの上限に適用されていた (§5.2.8 が group 単位で同一性を求める設計意図に
  反する)。
  - 購読する track ごとに、`renderGroup` が `Some(g)` なら group `g` に属する track、`None` なら
    その track 自身を対象に含め、その最大値を採る。購読する group が複数あるときは、時間軸が持てる値が
    1 つだけであるため最大値をすべての購読 track に掛ける。
  - 購読する track が 1 つも `renderGroup` を宣言していないとき (および `subscribed` が空のとき) は、
    group が分からないため従来どおり `tracks` の全 track の最大値を採る。
  - `is_live` による絞り込みは行わない (§5.2.8 の「isLive が FALSE ならこのフィールドを無視する」
    MUST は、カタログが必ず通る `MsfCatalogDocument::decode` の正規化で満たされている)。
- `CatalogResolution` の `target_latency_ms` を、選択した映像 / 音声 track の `renderGroup` と
  `targetLatency` (`SelectedTrackLatency`) に置き換え、`receive_catalog` は選択した track の情報だけを
  返すようにした。値の計算は `run` が `subscribe_video` / `subscribe_audio` を決めた後に行い、実際に
  購読する track の選択は純関数 `subscribed_track_latencies` に切り出した。
- `catalog_target_latency_ms` と `run` の doc を、group 単位で選ぶ理由と §5.2.8 / §5.2.11 の引用を
  含めて更新した。
- テストは `examples/moq-sub/src/pipeline.rs` の `#[cfg(test)] mod tests` を更新した。既存 5 件は
  新しい引数 (購読する track が `renderGroup` を宣言していない場合) に合わせ、購読する group だけを
  対象にすること、複数 group の最大値、片方だけ宣言した場合、購読しない group を除外すること、
  全未宣言のフォールバックを固定する 7 件を追加した。変異テストで、group 参照をやめる変更、分岐の
  量化子を反転する変更、購読判定の除外をやる変更がすべて検出されることを確認した。
- 検証は `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` の通過と、`/review-diff-code` 3 周 (致命的 0 件) で行った。

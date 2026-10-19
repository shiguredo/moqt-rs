# moq-sub のカタログ解釈を MSF に合わせる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-subscriber-catalog-handling
- Polished: {YYYY-MM-DD}

## 目的

moq-sub のカタログ解釈に、MSF-01 の MUST と実装上の取りこぼしがある。具体的には (1) `isLive=false` でも `targetLatency` を適用する (§5.2.8 は ignore を MUST)、(2) track 選択が codec の一致だけで `packaging` / `role` を見ない、(3) 独立カタログの置き換えで `isLive` / `isComplete` の履歴検証をしない (§5.2.7 の MUST)。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `catalog_target_latency_ms` は `t.target_latency` だけを見て `t.is_live` を見ない。
- 同じく `video_track` / `audio_track` の選択は codec の一致だけで、`MsfTrack::packaging` と `role` を条件にしていない。`eventtimeline` / `moqlog` / `moqmetrics` のトラックを LOC として購読し、デコードに失敗し得る。
- `examples/moq-sub/src/catalog.rs` のカタログ状態は独立カタログ (Full) を受けるたびに `self.catalog = Some(full.clone())` で置き換え、旧カタログとの `isLive` / `isComplete` の比較をしない。`isLive=false` の後に `isLive=true` の Full を受理する。
- `src/msf.rs` は delta 内の再追加で `isLive` の false→true を拒否するが、独立カタログ間の履歴は保持しない。
- 影響: VOD トラックに不要な遅延が入る。非 LOC トラックを LOC として購読して失敗する。MSF の MUST に違反するカタログ遷移を受理する。

## 設計方針

- `targetLatency` の適用候補から `is_live == false` のトラックを除外する。
- track 選択に `packaging == Loc` と `role` を加える。
- 独立カタログの置き換え時に旧トラックとの `isLive` / `isComplete` の遷移を検証し、違反は該当カタログを拒否してログに残す。
- テスト用 `track_with_target_latency` も `is_live` を引数に取る。

## 完了条件

- `isLive=false` の targetLatency が無視されること。
- 非 LOC トラックを購読しないこと。
- `isLive=false`→`isLive=true` の Full カタログ遷移が拒否されること。
- 上記を単体テストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

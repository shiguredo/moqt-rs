# example の細部の正しさを修正する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-small-fixes
- Polished: {YYYY-MM-DD}

## 目的

example に残る小さな正しさの欠陥 (キャッシュの非原子的更新、整数切り捨て、Group ID の飽和、加算の未検査、ログの誤出力) をまとめて直す。いずれも単体では小さいが、配信の正しさや調査に影響する。

## 現状

- `examples/moq-pub/src/cache.rs` の `MediaCache::insert` は properties の encode 前に evict するため、encode が失敗してもキャッシュ内容が変わる。テストは largest しか確認していない。
- `examples/moq-sub/src/pipeline.rs` の `extract_video_info` / `extract_audio_info` は width / height / framerate / samplerate を `as u32` で切り捨てる。`try_from` でエラーにすべきである。
- `examples/moq-pub/src/group_id.rs` の `next_initial_group_id` は `u64::MAX` で飽和し、同一 Group ID を再利用し得る。
- `examples/moq-pub/src/stream_writer.rs` の `SubgroupObjectState::on_skip` / `on_pass` は `+= 1` が未検査で、debug では panic し得る。
- `examples/moq-pub/src/pipeline.rs` の `serve_media_fetch` の警告ログは track 名ではなく `fetch.track_name.len()` を出す。
- `examples/moq-pub/src/catalog.rs` が publisher priority の `128` を直書きしており、`examples/moq-pub/src/pipeline.rs` の `DEFAULT_PUBLISHER_PRIORITY` と二重管理になっている。
- `examples/moq-sub/src/pipeline.rs` の `run` は `track_map` を snapshot してから `spawn_stream_task` 内で再度 `clone` しており、clone が二重になっている。
- 影響: 配信の Group 一意性・PTS 情報・障害調査の品質が損なわれる。

## 設計方針

- evict と properties encode の順序を入れ替え、失敗時にキャッシュを変えない。テストを追加する。
- 数値変換を `try_from` + エラーにし、失敗理由をログに残す。
- Group ID の飽和をエラーにする。加算は `checked_add` / `saturating_add` を意図に応じて使う。
- ログの値を track 名に直す。

## 完了条件

- 上記の各項目が修正され、必要なテストが追加されていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

# example の細部の不具合と二重管理を修正する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-example-small-fixes
- Polished: 2026-10-20

## 目的

example に残る小さな不具合 (キャッシュの非原子的更新、整数切り捨て、Group ID の飽和、加算の未検査、ログの誤出力) と、小さな二重管理 (publisher priority の直書き、track_map の二重 clone) をまとめて直す。いずれも単体では小さいが、配信の正しさ、障害調査、保守性に影響する。

## 現状

- `examples/moq-pub/src/cache.rs` の `MediaCache::insert` は properties の encode 前に evict するため、encode が失敗してもキャッシュ内容が変わる。テストは largest と保持 Object を確認するが、期限切れ Group の evict は確認していない。
- `examples/moq-sub/src/pipeline.rs` の `extract_video_info` / `extract_audio_info` は width / height / samplerate (u64) と framerate (f64) を `as u32` で変換する。u64 の値は下位 32 ビットに切り捨てられ、f64 の framerate は小数部を切り捨て、範囲外は飽和する。`u32::try_from` は u64 には使えるが f64 には実装されていない。
- `examples/moq-pub/src/group_id.rs` の `next_initial_group_id` は `u64::MAX` で飽和し、同一 Group ID を再利用し得る。`next_group_id` も同じく飽和する。`examples/moq-pub/src/pipeline.rs` の `video_group_id += 1` / `audio_group_id += 1` も未検査で、debug では panic、release では wrap して Group ID が 0 に戻り得る。
- `examples/moq-pub/src/stream_writer.rs` の `SubgroupObjectState::on_skip` の `self.next_object_id += 1` と `on_pass` の `self.next_object_id = object_id + 1` は未検査で、debug では panic、release では wrap して Object ID が重複し得る。
- `examples/moq-pub/src/pipeline.rs` の `serve_media_fetch` の警告ログは track 名ではなく `fetch.track_name.len()` を出す。
- `examples/moq-pub/src/catalog.rs` が publisher priority の `128` を直書きしており、`examples/moq-pub/src/pipeline.rs` の `DEFAULT_PUBLISHER_PRIORITY` と二重管理になっている。`DEFAULT_PUBLISHER_PRIORITY` は private なため、catalog から参照するには公開範囲を広げる必要がある。
- `examples/moq-sub/src/pipeline.rs` の `run` は `track_map` を snapshot してから `spawn_stream_task` 内で再度 `clone` しており、clone が二重になっている。
- 影響: encode 失敗時に FETCH 用の保持が変わり、Group ID の再利用・wrap で Location の一意性と単調増加が崩れ、整数切り捨てでデコード構成・MP4 のサンプル尺・ログが誤り、未検査の加算で debug は panic / release は wrap し、ログの誤出力で障害調査が誤誘導される。publisher priority の二重管理は片方だけの変更で priority の食い違いを生み、track_map の二重 clone は不要な割当てを生む。

## 設計方針

- evict と properties encode の順序を入れ替え、encode 失敗時にキャッシュを変えない。期限切れ Group と不正 Properties を同居させ、失敗後も Group が残ることを確認するテストを追加する。既存テストの doc コメントも新しい順序に合わせて更新する。
- width / height / samplerate は `u32::try_from` で変換し、範囲外はエラーにする。framerate は f64 のため u32 へ変換せず、`VideoTrackInfo` / `VideoSetup` の fps を f64 で保持し、MP4 の期待尺計算も f64 で行う。変換失敗の理由は既存のエラー伝播でログに残す。
- `next_initial_group_id` / `next_group_id` は `u64::MAX` でエラーにし、`video_group_id` / `audio_group_id` の更新も `next_group_id` を使ってエラーにする。`on_skip` / `on_pass` の Object ID 加算は `checked_add` でエラーにする。Group ID と Object ID の加算は飽和させない。
- ログの `track` は `fetch.track_name` を `String::from_utf8_lossy` で文字列化して出す。
- `DEFAULT_PUBLISHER_PRIORITY` を `pub(crate)` にし、`catalog.rs` から参照して `128` の直書きをなくす。
- `spawn_stream_task` の `track_map` 引数を所有権で受け取り、`run` の snapshot を move する。関数内の `clone` を削除する。

## 完了条件

- 上記の各項目が修正され、必要なテストが追加されていること。少なくとも、cache の evict 非実行、width / height / samplerate の範囲外エラー、framerate の小数保持、Group ID / Object ID の上限エラーを確認するテストを含むこと。catalog priority の定数参照、clone の削除、ログの track 名はコードレビューで確認する。
- 飽和を期待している既存テスト (`next_initial_group_id_saturates_at_the_maximum` / `next_group_id_increases_by_one` の `u64::MAX` ケース) はエラー期待に更新すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

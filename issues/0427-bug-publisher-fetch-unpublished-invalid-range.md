# moq-pub が Object を publish していない track の FETCH に INVALID_RANGE を返す

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publisher-fetch-unpublished-invalid-range
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-23 §3.2 (Fetch) は "If no Objects have been published for the track or Start Location is greater than the Largest Object (Section 3.1.4) the publisher MUST return FETCH_ERROR with error code INVALID_RANGE." と定める。
moq-pub は配信中の track でも、まだ 1 件も Object を cache に入れていない場合の FETCH に `REQUEST_DOES_NOT_EXIST` を返し、この MUST を満たさない。

## 現状

- `crates/moq-pub/src/pipeline.rs` の `serve_peer_request` は、FETCH の Full Track Name を `published_track` で照合し、配信していない track (別 namespace を含む) を `REQUEST_DOES_NOT_EXIST` で拒否する。`serve_media_fetch` に届くのは配信中の映像 / 音声 track だけである。
- `crates/moq-pub/src/cache.rs` の `MediaCache::plan` は、track の entry が cache に無いと `FetchPlan::UnknownTrack` を返す。`serve_media_fetch` は `UnknownTrack` を `REQUEST_DOES_NOT_EXIST` で拒否する。
  `serve_media_fetch` に届く時点で track は配信中なので、`UnknownTrack` は「配信中だが Object を 1 件も cache に入れていない」ことを意味する。
- `FetchPlan::NoPublishedObjects` (`REQUEST_INVALID_RANGE` で拒否する) は、`TrackCache` が `MediaCache::insert` の中でしか作られず、作るときに `first_published` を必ず設定するため到達しない。
- Session は、publisher 役の購読があって Largest Object をまだ観測していない場合は自分で INVALID_RANGE を返す (`src/session/fetch.rs`)。moq-pub まで届くのは、その購読が無い場合である。

## 設計方針

- `serve_media_fetch` に届いた FETCH で、対象 track の Object を 1 件も cache に入れていない場合は `REQUEST_INVALID_RANGE` で拒否する (§3.2)。
- `MediaCache::plan` の `UnknownTrack` と到達しない `NoPublishedObjects` を、「Object を 1 件も publish していない」を表す 1 つの結果に統合し、doc とテストを合わせる。
- 配信していない track の FETCH は従来どおり `serve_peer_request` が `REQUEST_DOES_NOT_EXIST` で拒否する。

## 完了条件

- 配信中だが Object を 1 件も cache に入れていない track への FETCH が `REQUEST_INVALID_RANGE` で拒否されること。
- 配信していない track (別 namespace を含む) への FETCH は従来どおり `REQUEST_DOES_NOT_EXIST` で拒否されること。
- `MediaCache::plan` の結果の型と `crates/moq-pub/src/cache.rs` のテストが新しい分類に合っていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

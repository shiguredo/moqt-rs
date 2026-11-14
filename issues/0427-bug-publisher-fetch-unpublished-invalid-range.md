# moq-pub が Object を publish していない track の FETCH に INVALID_RANGE を返す

- Created: 2026-11-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publisher-fetch-unpublished-invalid-range
- Polished: 2026-11-14

## 目的

draft-ietf-moq-transport-23 §3.2 (Fetch) は "If no Objects have been published for the track or Start Location is greater than the Largest Object (Section 3.1.4) the publisher MUST return FETCH_ERROR with error code INVALID_RANGE." と定める。
moq-pub は配信中の track でも、まだ 1 件も Object を cache に入れていない場合の FETCH に `REQUEST_DOES_NOT_EXIST` を返す。track は存在するため、§3.2 の INVALID_RANGE を返す必要がある。

## 現状

- `crates/moq-pub/src/pipeline.rs` の `serve_peer_request` は、FETCH の Full Track Name を `published_track` で照合し、配信していない track (別 namespace を含む) を `REQUEST_DOES_NOT_EXIST` で拒否する。`serve_media_fetch` に届くのは配信中の映像 / 音声 track だけである。
- `crates/moq-pub/src/cache.rs` の `MediaCache::plan` は、`GROUP_ORDER` が Descending でなければ、track の entry が cache に無いと `FetchPlan::UnknownTrack` を返す。`serve_media_fetch` は `UnknownTrack` を `REQUEST_DOES_NOT_EXIST` で拒否する。
  `serve_media_fetch` に届く時点で track は配信中なので、`UnknownTrack` は「配信中だが Object を 1 件も cache に入れていない」ことを意味する。
- `FetchPlan::NoPublishedObjects` (`REQUEST_INVALID_RANGE` で拒否する) は、`TrackCache` が `MediaCache::insert` の中でしか作られず、作るときに `first_published` を必ず設定するため到達しない。
- `MediaCache` に入るのは PUBLISH 購読の writer がフィルタを通過させた Object だけであり、peer の SUBSCRIBE に送った Object は入らない (`crates/moq-pub/src/pipeline.rs` の `media_cache.insert`)。
  Session は、publisher 役の購読があり Track の Largest Object をまだ観測していない場合は自分で INVALID_RANGE を返す (`src/session/fetch.rs`)。
  Session が受理した FETCH だけが moq-pub に届く。Largest Object は Track 単位で、別の publisher 役購読が送った Object でも観測済みになるため、`MediaCache` が空でも FETCH が受理され得る。

## 設計方針

- `serve_media_fetch` に届いた FETCH で、対象 track の Object を 1 件も cache に入れていない場合は `REQUEST_INVALID_RANGE` で拒否する (§3.2)。
  `MediaCache::plan` の no-objects 判定は `GROUP_ORDER` の判定より先に行い、Descending の要求でも同じ結果にする (Session の受理判定と同じく、Object が無いことを優先する)。
- `MediaCache::plan` の `UnknownTrack` と到達しない `NoPublishedObjects` を、「cache に Object を 1 件も入れていない」を表す 1 つの結果に統合し、doc とテストを合わせる。
- 配信していない track の FETCH は従来どおり `serve_peer_request` が `REQUEST_DOES_NOT_EXIST` で拒否する。

## 完了条件

- 配信中だが Object を 1 件も cache に入れていない track への FETCH が `REQUEST_INVALID_RANGE` で拒否されること。
  `GROUP_ORDER` が Descending の要求でも同じであること。
- 配信していない track (別 namespace を含む) への FETCH は従来どおり `REQUEST_DOES_NOT_EXIST` で拒否されること。
- `MediaCache::plan` の結果の型と `crates/moq-pub/src/cache.rs` のテストが新しい分類に合っていること。
  `crates/moq-pub/src/pipeline.rs` の `FetchPlan::UnknownTrack` への doc 参照も更新されていること。
- `crates/moq-pub/README.md` の FETCH と `--cache-duration` の説明が、cache に Object が 1 件も無い track の `REQUEST_INVALID_RANGE` に合っていること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

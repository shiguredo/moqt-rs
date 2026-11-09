# moq-pub が SUBSCRIBE / FETCH の Track Namespace を検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: 2026-11-10
- Refreshed: 2026-11-04

## 目的

Track は Track Namespace と Track Name からなる Full Track Name で識別される (draft-ietf-moq-transport-23 §2.4.1 (Track Naming))。SUBSCRIBE (§9.6) と FETCH (§9.11) はどちらも Track Namespace と Track Name を運ぶ。moq-pub は受信 SUBSCRIBE / FETCH の track name だけを見て受理するため、別 namespace の要求に自分の namespace のコンテンツを返し得る。誤った配信を防ぐ。

## 現状

- `crates/moq-pub/src/pipeline.rs` の `serve_peer_request` は、SUBSCRIBE を `let track: &[u8] = &subscribe.track_name;` と `config.video_enabled && track == config.track_name.as_bytes()` で判定し、`subscribe.track_namespace` を比較しない。
- FETCH も `if fetch.track_name == MSF_CATALOG_TRACK_NAME` と
  `config.video_enabled && track_name == config.track_name.as_bytes()` で catalog / メディアを判定し、
  `fetch.track_namespace` を見ない。`serve_media_fetch` は `media_cache.largest(&config.namespace, track_name)` と
  `media_cache.plan(published, &config.namespace, track_name, ...)` で常に `config.namespace` の cache を引く。
- draft-ietf-moq-transport-23 §2.4.1 (Track Naming) は Track Name を 1 バイト以上とし、長さ 0 の Track Name の受信に `PROTOCOL_VIOLATION` を要求する。Session は SUBSCRIBE / FETCH の decode 時に `src/message/common.rs` の `validate_track_name` でこれを検証するため、`serve_peer_request` に追加する検証は namespace の一致だけである。
- 影響: 別 namespace の SUBSCRIBE を受理して SUBSCRIBE_OK を返し、別 namespace の FETCH には自分の namespace のコンテンツを返し得る。relay 経由でも起こり得る。
- REQUEST_UPDATE は Track Namespace を運ばず、対象の request と同じ bidi stream で送られて、その request に紐づく (draft-ietf-moq-transport-23 §9.5 (REQUEST_UPDATE) の "send a REQUEST_UPDATE on the same bidi stream as the request to modify it")。`SessionEvent::RequestUpdateReceived` の `request_id` は対象 request の Request ID である。
- 現在の `client.subscription_track_name(update.request_id)` による映像購読の判定は track 名だけを見る。しかし、subscription に対する
  REQUEST_UPDATE が届くのは Established の subscription だけであり、Established になるのは SUBSCRIBE を受理したものか、自側が
  `config.namespace` で PUBLISH したものだけである。したがって追加の namespace 検証は不要である。FETCH への REQUEST_UPDATE は namespace を持たず、
  コンテンツの namespace 判定にも関与しない。
- FILL_PARAMETERS 付き SUBSCRIBE で fill fetch stream を開く条件を満たす場合、Session は SUBSCRIBE を Pending として受理した直後、アプリが応答する前に `OpenFillFetchStream` を発行する。
- `MoqtClient::next_event` は `IncomingRequest` を `OpenFillFetchStream` より先に返し、`serve_peer_request` が SUBSCRIBE
  を拒否すると subscription は Terminated になる。その後の fill 応答は `send_fill_fetch_header` が Terminated を拒否するため、
  コンテンツは送られない。fill 経路の追加変更は不要である (draft-ietf-moq-transport-23 §3.4.1 (Opening and Closing Fill Fetch Streams))。

## 再現手順

1. `crates/moq-pub` を relay に接続し、`--namespace moq-example` で video / audio / catalog を PUBLISH する。
2. relay から、`Track Namespace = other`、`Track Name = catalog` の FETCH を送る。`serve_peer_request` は namespace を比較せず catalog の FETCH_OK と Object を返す。
3. 同じく `Track Namespace = other`、`Track Name = video` (または audio) の SUBSCRIBE を送る。`serve_peer_request` は namespace を比較せず SUBSCRIBE_OK を返す。
4. `Track Namespace = other`、`Track Name = video` (または audio) の FETCH を送る。`serve_media_fetch` は `config.namespace` の cache を引くため、video / audio の保持 Object を返す。

## 設計方針

- `crates/moq-pub/src/pipeline.rs` に private な
  `fn published_track(config: &Config, namespace: &TrackNamespace, name: &[u8]) -> Option<PublishedTrack>`
  を追加する。`PublishedTrack` は `Video` / `Audio` / `Catalog` の enum とする。`namespace` が `config.namespace`
  と完全一致しない場合は `None` を返す。一致する場合は `config.video_enabled` / `config.audio_enabled` と track 名から種別を返す。namespace
  が一致する場合、catalog は `config.video_enabled` / `audio_enabled` に関係なく `Some(Catalog)` とする。
- `serve_peer_request` の SUBSCRIBE 分岐の先頭で
  `published_track(config, &subscribe.track_namespace, &subscribe.track_name)` を呼ぶ。`None` なら既存の `None` 分岐と同じく
  `send_request_error(request_id, REQUEST_DOES_NOT_EXIST, "unknown track")` で拒否し、`Some(track)` なら種別に応じて既存の
  track alias / SUBSCRIBE_OK / NEW_GROUP_REQUEST の処理へ進む。
- FETCH 分岐の先頭でも同じヘルパを呼び、`None` なら `REQUEST_DOES_NOT_EXIST` で拒否する。catalog の分岐もこの検証を通す。`Some(Catalog)` のときだけ catalog の FETCH 処理へ、`Some(Video)` / `Some(Audio)` のときだけ `serve_media_fetch` へ進む。
- `serve_media_fetch` の呼び出し元は `serve_peer_request` の FETCH 分岐だけであり、上記の検証の後は namespace が `config.namespace` と一致し、track は配信中の video / audio に限られる。そのため `serve_media_fetch` では namespace を再比較せず、`media_cache.largest` / `plan` には従来どおり `&config.namespace` を渡す。
- `serve_media_fetch` の `published` の計算 (`(config.video_enabled && track_name == config.track_name.as_bytes()) || ...`) と、`crates/moq-pub/src/cache.rs` の `MediaCache::plan` の `published` 引数と `if !published { return FetchPlan::UnknownTrack; }` は、未知の track が `serve_peer_request` で拒否されるため常に真になり到達しなくなる。到達しない分岐を残さないよう、両方を削除する。
  - `FetchPlan::UnknownTrack` は「配信中だが cache に 1 件も保持していない track」でも返るため残す。`MediaCache::plan` の doc から `published` の説明を除く。
  - `cache.rs` のテストは、`plan_reports_an_unpublished_track_as_unknown` を保持の無い track だけの検証にし、`published = false` を使う `plan_checks_the_track_before_the_group_order` は削除する (対象外の track を GROUP_ORDER より先に判定する順序は `serve_peer_request` の先頭の検証が保証する)。
- REQUEST_UPDATE には namespace の検証を追加しない。REQUEST_UPDATE は namespace を運ばず、subscription に対する REQUEST_UPDATE が届くのは
  Established の subscription だけであり、Established になるのは SUBSCRIBE の受理時に namespace を検証済みの subscription か、自側が
  `config.namespace` で PUBLISH した subscription だけである。`subscription_track_name` による映像購読の判定はその前提で安全である。
  `DOES_NOT_EXIST` (コード定数 `REQUEST_DOES_NOT_EXIST`、0x10) は draft-ietf-moq-transport-23 §12.3 (Request Error Codes) で publisher が SUBSCRIBE /
  FETCH / TRACK_STATUS / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS への応答に使うコードであり、REQUEST_UPDATE への応答には使わない。
- `crates/moq-pub/src/pipeline.rs` の `#[cfg(test)] mod tests` に `published_track` の単体テストを追加する。`Config` は
  `cli::parse_args` で組み立てる。別 namespace の video / audio / catalog が `None` になること、同じ namespace の有効な video /
  audio / catalog がそれぞれ `Some(Video)` / `Some(Audio)` / `Some(Catalog)` になること、無効な video / audio が `None`
  になることを固定する。
- handler の配線 (REQUEST_DOES_NOT_EXIST の送出) は `serve_peer_request` のコードで確認する。pipeline 全体の統合テストは本 issue の範囲外とする (0338 でテスト基盤を追加する)。
  0338 の moq-pub 統合テストの一覧は「未知の track が REQUEST_ERROR(REQUEST_DOES_NOT_EXIST) になること」だけで、別 namespace の要求を含まない。統合経路での回帰テストには、0338 の一覧へ別 namespace の SUBSCRIBE / FETCH を加える必要がある。

## 完了条件

- 別 namespace の SUBSCRIBE / FETCH (catalog / video / audio) が `DOES_NOT_EXIST` (`REQUEST_DOES_NOT_EXIST`、0x10) で拒否されること。
- 自分の namespace の SUBSCRIBE / FETCH (catalog / video / audio) は従来どおり受理されること。
- 上の 2 つは、`published_track` の単体テストと、`serve_peer_request` の SUBSCRIBE / FETCH の両分岐が `published_track` の結果だけで受理と拒否を分けることのコード確認で満たす。
- `MediaCache::plan` の `published` 引数と `serve_media_fetch` の `published` の計算が削除され、`cache.rs` のテストが追随していること。
- `published_track` の単体テストが追加され、別 namespace の要求と配信していない track (無効な video / audio を含む) が `None` になり、同じ namespace の有効な track が正しい種別になること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

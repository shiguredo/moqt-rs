# moq-pub が SUBSCRIBE / FETCH の Track Namespace を検証する

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publisher-namespace-validation
- Polished: 2026-10-20

## 目的

Track は Track Namespace と Track Name からなる Full Track Name で識別される (draft-ietf-moq-transport-22 §2.4.1 (Track Naming))。SUBSCRIBE (§9.6) と FETCH (§9.11) はどちらも Track Namespace と Track Name を運ぶ。moq-pub は受信 SUBSCRIBE / FETCH の track name だけを見て受理するため、別 namespace の要求に自分の namespace のコンテンツを返し得る。誤った配信を防ぐ。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` は、SUBSCRIBE を `let track: &[u8] = &subscribe.track_name;` と `config.video_enabled && track == config.track_name.as_bytes()` で判定し、`subscribe.track_namespace` を比較しない。
- FETCH も `if fetch.track_name == MSF_CATALOG_TRACK_NAME` と
  `config.video_enabled && track_name == config.track_name.as_bytes()` で catalog / メディアを判定し、
  `fetch.track_namespace` を見ない。`serve_media_fetch` は `media_cache.largest(&config.namespace, track_name)` と
  `media_cache.plan(published, &config.namespace, track_name, ...)` で常に `config.namespace` の cache を引く。
- 影響: 別 namespace の SUBSCRIBE を受理して SUBSCRIBE_OK を返し、別 namespace の FETCH には自分の namespace のコンテンツを返し得る。relay 経由でも起こり得る。
- REQUEST_UPDATE は Track Namespace を運ばず、既存の request (SUBSCRIBE / FETCH など) の Request ID に紐づく (draft-ietf-moq-transport-22 §9.5 (REQUEST_UPDATE))。`SessionEvent::RequestUpdateReceived` の `request_id` は対象 request の Request ID である。
- 現在の `client.subscription_track_name(update.request_id)` による映像購読の判定は track 名だけを見る。しかし、subscription に対する
  REQUEST_UPDATE が届くのは Established の subscription だけであり、Established になるのは SUBSCRIBE を受理したものか、自側が
  `config.namespace` で PUBLISH したものだけである。したがって追加の namespace 検証は不要である。FETCH への REQUEST_UPDATE は namespace を持たず、
  コンテンツの namespace 判定にも関与しない。
- FILL_PARAMETERS 付き SUBSCRIBE で fill fetch stream を開く条件を満たす場合、Session は SUBSCRIBE を Pending として受理した直後、アプリが応答する前に `OpenFillFetchStream` を発行する。
- `MoqtClient::next_event` は `IncomingRequest` を `OpenFillFetchStream` より先に返し、`serve_peer_request` が SUBSCRIBE
  を拒否すると subscription は Terminated になる。その後の fill 応答は `send_fill_fetch_header` が Terminated を拒否するため、
  コンテンツは送られない。fill 経路の追加変更は不要である (draft-ietf-moq-transport-22 §3.4.1 (Opening and Closing Fill Fetch Streams))。

## 再現手順

1. `examples/moq-pub` を relay に接続し、`--namespace moq-example` で video / audio / catalog を PUBLISH する。
2. relay から、`Track Namespace = other`、`Track Name = catalog` の FETCH を送る。`serve_peer_request` は namespace を比較せず catalog の FETCH_OK と Object を返す。
3. 同じく `Track Namespace = other`、`Track Name = video` (または audio) の SUBSCRIBE を送る。`serve_peer_request` は namespace を比較せず SUBSCRIBE_OK を返す。
4. `Track Namespace = other`、`Track Name = video` (または audio) の FETCH を送る。`serve_media_fetch` は `config.namespace` の cache を引くため、video / audio の保持 Object を返す。

## 設計方針

- `examples/moq-pub/src/pipeline.rs` に private な
  `fn published_track(config: &Config, namespace: &TrackNamespace, name: &[u8]) -> Option<PublishedTrack>`
  を追加する。`PublishedTrack` は `Video` / `Audio` / `Catalog` の enum とする。`namespace` が `config.namespace`
  と完全一致しない場合は `None` を返す。一致する場合は `config.video_enabled` / `config.audio_enabled` と track 名から種別を返す。namespace
  が一致する場合、catalog は `config.video_enabled` / `audio_enabled` に関係なく `Some(Catalog)` とする。
- `serve_peer_request` の SUBSCRIBE 分岐の先頭で
  `published_track(config, &subscribe.track_namespace, &subscribe.track_name)` を呼ぶ。`None` なら既存の `None` 分岐と同じく
  `send_request_error(request_id, REQUEST_DOES_NOT_EXIST, "unknown track")` で拒否し、`Some(track)` なら種別に応じて既存の
  track alias / SUBSCRIBE_OK / NEW_GROUP_REQUEST の処理へ進む。
- FETCH 分岐の先頭でも同じヘルパを呼び、`None` なら `REQUEST_DOES_NOT_EXIST` で拒否する。catalog の分岐もこの検証を通す。`Some(Catalog)` のときだけ catalog の FETCH 処理へ、`Some(Video)` / `Some(Audio)` のときだけ `serve_media_fetch` へ進む。
- `serve_media_fetch` の先頭でも `fetch.track_namespace` と `config.namespace` が完全一致するかどうかを比較し、不一致なら `REQUEST_DOES_NOT_EXIST` で拒否する。一致する場合だけ `media_cache.largest` / `plan` に `&fetch.track_namespace` を渡す。
- REQUEST_UPDATE には namespace の検証を追加しない。REQUEST_UPDATE は namespace を運ばず、subscription に対する REQUEST_UPDATE が届くのは
  Established の subscription だけであり、Established になるのは SUBSCRIBE の受理時に namespace を検証済みの subscription か、自側が
  `config.namespace` で PUBLISH した subscription だけである。`subscription_track_name` による映像購読の判定はその前提で安全である。
  `REQUEST_DOES_NOT_EXIST` は draft-ietf-moq-transport-22 §12.3 (Request Error Codes) で publisher が SUBSCRIBE /
  FETCH / TRACK_STATUS / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS への応答に使うコードであり、REQUEST_UPDATE への応答には使わない。
- `examples/moq-pub/src/pipeline.rs` の `#[cfg(test)] mod tests` に `published_track` の単体テストを追加する。`Config` は
  `cli::parse_args` で組み立てる。別 namespace の video / audio / catalog が `None` になること、同じ namespace の有効な video /
  audio / catalog がそれぞれ `Some(Video)` / `Some(Audio)` / `Some(Catalog)` になること、無効な video / audio が `None`
  になることを固定する。
- handler の配線 (REQUEST_DOES_NOT_EXIST の送出) は `serve_peer_request` のコードで確認する。pipeline 全体の統合テストは本 issue の範囲外とする (0338 でテスト基盤を追加する)。

## 完了条件

- 別 namespace の SUBSCRIBE / FETCH (catalog / video / audio) が `REQUEST_DOES_NOT_EXIST` で拒否されること。
- 自分の namespace の SUBSCRIBE / FETCH (catalog / video / audio) は従来どおり受理されること。
- `published_track` の単体テストが追加され、別 namespace の要求と配信していない track (無効な video / audio を含む) が `None` になり、同じ namespace の有効な track が正しい種別になること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

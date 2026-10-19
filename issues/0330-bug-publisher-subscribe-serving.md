# moq-pub が受信 SUBSCRIBE の購読に Object と PUBLISH_DONE を送る

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-publisher-subscribe-serving
- Polished: {YYYY-MM-DD}

## 目的

moq-pub は relay からの SUBSCRIBE を受理して SUBSCRIBE_OK を返すが、その request_id を保存せず、Object も PUBLISH_DONE も送らない。結果として受信 SUBSCRIBE の購読は確立するだけで何も届かず、draft-ietf-moq-transport-22 §3.1.1 (paused でない購読には Object を送る)、§3.3.3 (フィルタを通る Object だけを forward)、§6.4.2.2 / §9.9 (Established subscription の
publisher は PUBLISH_DONE を送る) を満たさない。

## 現状

- `examples/moq-pub/src/pipeline.rs` の `serve_peer_request` は SUBSCRIBE を受理し、`send_subscribe_ok` を呼ぶが request_id を保存せず、writer / datagram の送出に使わない。
- 送出 subgroup stream / datagram / PUBLISH_DONE は、自側が送った PUBLISH の `video_request_id` / `audio_request_id` / `catalog_request_id` に固定されている。
- `src/session/data.rs` の `send_subgroup_object` は stream が保持する request_id の subscription 1 個だけを `object_passes_filters` に渡す。そのため受信 SUBSCRIBE の LOCATION_FILTER / Range Filter / `FORWARD=0` は評価されず、PUBLISH_DONE の Stream Count も PUBLISH 側にしか計上されない。
- 影響: relay が SUBSCRIBE を転送する構成で、購読者に Object が届かない。pause も効かない。テストは無い。

## 設計方針

- 受信 SUBSCRIBE の request_id を track ごとに保持し、購読ごとの stream / datagram を送る。
- 1 本の stream を複数購読で共有する設計を維持する場合は、フィルタ付き・Forward 0 の SUBSCRIBE を明示的に `REQUEST_NOT_SUPPORTED` で拒否し、その判断を doc に残す。
- PUBLISH_DONE は各受信 SUBSCRIBE の bidi request stream に正しい Stream Count で送る。
- 実リレー E2E でフィルタ付き購読と pause を確認する。

## 完了条件

- 受信 SUBSCRIBE で確立した購読に Object が届くこと (フィルタ・Forward を反映するか、明示的に拒否すること)。
- 各購読に PUBLISH_DONE が送られること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

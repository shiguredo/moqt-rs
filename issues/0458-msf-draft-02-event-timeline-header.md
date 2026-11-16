# MSF_EVENT_TIMELINE_HEADER track property に対応する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §8.1 (Event Timeline header data) は、Event Timeline track の header data を新しい Track property で運ぶ。
§12.3.1 (MSF_EVENT_TIMELINE_HEADER Track property) は property type を 0x7B とし、使用する場合は publisher が `PUBLISH` または `SUBSCRIBE_OK` / `FETCH_OK` に含めること (MUST)、subscriber が payload を解析する前に property の有無を確認すること (MUST) を定める。
現行実装はこの property を持たず、header data を運ぶ経路も、subscriber が確認する経路もない。

## 現状

- `src/track_properties.rs` と `src/property_scope.rs` に MSF_EVENT_TIMELINE_HEADER に相当する定数・accessor がない。`known_property_scope` は 0x7B を未知 property として扱う。
- `TrackProperties` は汎用の property を保持でき、`Subscription::peer_track_properties` から参照できる。MSF 用の解釈はない。
- `MsfEventTimeline::decode` は entry 列だけを扱い、header data を引数に取らない。
- v02 §8.1 は header data が optional であり、存在しない場合は header data なしとする。値の serialization は `eventType` が定義し、track の寿命を通じて不変である。
- 0x7B は [MOQT] §8.4 の application-specific range (0x78〜0x7F) にあり、IANA の割当待ちではない (A.1.8)。

## 設計方針

- `PROP_MSF_EVENT_TIMELINE_HEADER = 0x7B` を定義し、`known_property_scope` に Track property として登録する。重複 instance を許さない検証も加える。
- `TrackProperties` から header data の生バイト列を取り出す accessor を追加する。library は eventType ごとの構造を知らないため、値は不透明なバイト列として扱う。
- subscriber 側で「payload を parse する前に property の有無を確認する」ための入口を追加する。header data を必要とする eventType の decode は、property がない場合に `MessageError::InvalidCatalog` を返せるようにする。
- publisher 側で `PUBLISH` / `SUBSCRIBE_OK` / `FETCH_OK` に property を載せる経路は、実際に Event Timeline を配信する実装が入った時点で接続する。本 issue では property の定義・取得・検証までを対象にする。

## 完了条件

- 0x7B が Track property として認識され、`TrackProperties` から値を取得できること。
- 同じ track で 0x7B が複数回現れる property 集合が拒否されること。
- header data を必要とする decode が、property なしの場合に失敗し、property ありの場合に成功すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

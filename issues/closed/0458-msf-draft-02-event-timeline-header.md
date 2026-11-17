# MSF_EVENT_TIMELINE_HEADER track property に対応する

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §8.1 (Event Timeline header data) は、Event Timeline track の header data を新しい Track property で運ぶ。
§12.3.1 (MSF_EVENT_TIMELINE_HEADER Track property) は property type を 0x7B とし、使用する場合は publisher が `PUBLISH` または `SUBSCRIBE_OK` / `FETCH_OK` に含めること (MUST)、subscriber が payload を解析する前に property の有無を確認すること (MUST) を定める。header data がない場合、property は存在しない。

現行実装はこの property を持たず、header data を運ぶ経路も、subscriber が確認する経路もない。

## 現状

- `src/property_scope.rs` の `known_property_scope` は 0x7B を未知 property として扱う。
- `src/track_properties.rs` の `TrackProperties` は汎用の property を保持でき、`find_bytes` で任意型の生バイトを取得できる。MSF 用の解釈はない。
- `MsfEventTimeline::decode` は entry 列だけを扱い、header data を引数に取らない。
- `crates` の `Subscription::peer_track_properties` から PUBLISH / SUBSCRIBE_OK の Track Properties は参照できる。一方、FETCH_OK の Track Properties は `handle_ok_for_fetch_initial` で `validate()` されるだけで、`Fetch` にも `SessionEvent::FetchOkReceived` にも保持されない。
- 0x7B は [MOQT] §8.4 の application-specific range (0x78〜0x7F) にあり、IANA の割当待ちではない (A.1.8)。

## 設計方針

- `PROP_MSF_EVENT_TIMELINE_HEADER = 0x7B` を定義し、`known_property_scope` に Track property として登録する。重複 instance の拒否は既存の `TrackProperties::validate_multiplicity` に任せる。
- `TrackProperties::event_timeline_header() -> Option<&[u8]>` を追加し、header data の生バイト列を返す。
- `MsfEventTimeline::decode_with_header(buf, header: &[u8])` を公開 API として追加する。header data の解釈は eventType が定義するが、library は eventType を知らないため、header が必須かどうかの判定は呼び出し側が行う。property がない場合は「header data なし」であり、decode を失敗させない。
- FETCH_OK の Track Properties を `Fetch` または `SessionEvent::FetchOkReceived` に保持し、subscriber が FETCH 経由でも 0x7B を確認できるようにする。
- publisher が `send_publish` / `send_subscribe_ok` / `send_fetch_ok` で 0x7B を載せる経路は既存の `TrackProperty` で表現できる。Event Timeline を実際に配信する実装が入った時点で接続する。本 issue では property の定義・取得・保持・decode API までを対象にする。

## 完了条件

- 0x7B が Track property として認識され、`TrackProperties::event_timeline_header()` から値を取得できること。
- 同じ track で 0x7B が複数回現れる property 集合が `TrackProperties::validate_multiplicity` で拒否されること。
- `MsfEventTimeline::decode_with_header` が header ありで成功し、header なしの場合は呼び出し側が「header data なし」として扱えること (decode 自体は失敗しない)。
- FETCH_OK の Track Properties が `Fetch` または `SessionEvent::FetchOkReceived` に保持され、subscriber が取得できること。
- header data が track の寿命を通じて不変であることを doc に明記すること。
- 上記を `src/track_properties.rs`、`src/property_scope.rs`、`src/msf.rs`、`src/session/fetch.rs` のテストで固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `PROP_MSF_EVENT_TIMELINE_HEADER = 0x7B` を追加し、`known_property_scope` に Track property として登録した。`TrackProperties::event_timeline_header()` で生バイト列を取得する。重複は既存の `validate_multiplicity` が malformed Track として拒否する。
- `MsfEventTimeline::decode_with_header(buf, header)` を追加した。header の解釈は eventType の責務であり、library は entry 列だけを decode する。header なしでも失敗しない。
- `Fetch` に `track_properties: TrackProperties` を追加し、FETCH_OK 受信時に `RequestOk.track_properties` を保持するようにした。publisher 側も `send_fetch_ok` で送信した値を保持する。
- header data が track の寿命を通じて不変であることを property / accessor の doc に明記した。
- `tests/test_track_properties.rs` / `tests/test_msf/event_timeline.rs` / `tests/test_session/parameter_rules.rs` に accessor、重複拒否、header 付き decode、FETCH_OK の保持を追加した。`pbt/tests/prop_object_properties.rs` は 0x7B を Object scope の生成から除外した。`docs/msf.md` も更新した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。

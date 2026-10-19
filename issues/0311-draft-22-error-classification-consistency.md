# MessageError の分類と表示を一貫させる

- Created: 2026-10-19
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-error-classification-consistency
- Polished: 2026-10-19

## 目的

draft-ietf-moq-transport-22 §10.7 (Immutable Properties) は、Object の IMMUTABLE_PROPERTIES の内側に別の IMMUTABLE_PROPERTIES を含む Track を malformed (§12.1 (Malformed Tracks)) と定める。§8.3 (Key-Value-Pair Structure) は、既知型の Value または Length/Value が型の定義と一致しない場合に KEY_VALUE_FORMATTING_ERROR、値長が 2^16-1 バイトを超える場合に PROTOCOL_VIOLATION でセッションを閉じることを MUST で求める。現状の `MessageError` は同じ条件で variant が揃わず、`Display` も二重接頭辞になる。エラー型で購読 cancel とセッション終了を分岐する利用者が正しく判断できない。

## 現状

- `src/object_properties.rs` の `decode_kv_pairs` は入れ子 IMMUTABLE_PROPERTIES を `ProtocolViolation` で返すが、同じファイルの Prior Gap 系は `MalformedTrack` で返す。`ObjectProperties::decode_exact` は KVP 層の失敗や末尾の余分バイトを区別せず `ProtocolViolation("invalid object properties framing")` に写すため、`ObjectPropertyTracker::observe_object` と `src/video_decode_order.rs` の `prior_object_id_gap_of` も `ProtocolViolation` を返す。
- `src/track_properties.rs` の `decode` / `encode` は入れ子 IMMUTABLE_PROPERTIES を `ProtocolViolation` で返す。§10.7 の malformed 条件は Object を主語にしており、`decode_track_kv_pairs` の doc は入れ子を malformed と書きながら `ProtocolViolation` を返しているため、分類の根拠が説明されていない。
- `src/error.rs` の `MessageError::MalformedTrack` の `Display` が "malformed track: " を前置する一方、生成側のメッセージも "malformed track: ..." で始まるものが多く、二重接頭辞になる。`src/stream/decoder.rs` の `validate_fetch_object` が返す `ProtocolViolation` にも "malformed track: ..." で始まるものがあり、`Display` が "protocol violation: malformed track: ..." になる。0310 はこの文言の不整合を本 issue では扱わないと明記しており、本 issue で解消する。
- `src/object_properties.rs` / `src/track_properties.rs` / `src/loc.rs` の encode は Properties の KVP 値長 65535 超を `MessageError::PayloadTooLong` で返すが、decode は同じ条件を `ProtocolViolation` で返す。`src/message_parameter.rs` / `src/parameter.rs` は encode / decode とも `ProtocolViolation` である。0020 は MessageParameters / SetupOptions の encode を `ProtocolViolation` に揃えたが Properties 系は対象外のままで、0238 も Properties 系を対象外と明記している。
- 影響: 公開 `ObjectProperties::decode` の入れ子 IMMUTABLE_PROPERTIES の分類が §10.7 / §12.1 の Must-cancel と一致しない。`TrackProperties::decode` は §10.7 の malformed 条件の対象外であり、Track 側の分類を決める必要がある。`Display` の文言も不正確になる。Properties の KVP 値長超過は encode と decode で variant が異なる。

## 設計方針

- Object 側の入れ子 IMMUTABLE_PROPERTIES は decode で `MalformedTrack` に統一する。`ObjectProperties::encode` はローカル入力の不正であり受信した Track の判定ではないため、`ProtocolViolation` のまま据え置く。`decode_kv_pairs` の呼び出し文脈で分類を分ける。
- `ObjectProperties::decode_exact` は framing の失敗を `ProtocolViolation` に畳む現行方針を維持しつつ、`MalformedTrack` はそのまま返す。これにより `ObjectPropertyTracker::observe_object` と `prior_object_id_gap_of` も入れ子 IMMUTABLE_PROPERTIES を `MalformedTrack` として返す。`decode_exact` と `prior_object_id_gap_of` の doc、`tests/test_session/data_stream.rs` の reason 期待値を更新する。
- Track 側の入れ子 IMMUTABLE_PROPERTIES は `ProtocolViolation` のまま据え置く。現行実装は §10.7 冒頭が IMMUTABLE_PROPERTIES の中身を Track or Object Property と対称に定義していることを根拠に、Track scope でも入れ子を禁止している。本 issue はこの禁止判断を変えず、malformed track の条件が Object に限られるため `MalformedTrack` ではなく `ProtocolViolation`（セッション終了）とする。`decode_track_kv_pairs` の doc にこの判断と根拠を明記する。
- `Display` の二重接頭辞を解消し、`MessageError` の生成側メッセージから "malformed track: " を外す。対象は `MessageError::MalformedTrack` の生成箇所と、`validate_fetch_object` の `ProtocolViolation` のうち "malformed track: " で始まるもの。`Display` が variant ごとの接頭辞を 1 つだけ付ける。`MessageError::reason()` は生成側の文字列をそのまま返すため、`MessageError::reason()` 経由の `SessionError::reason` から "malformed track: " 接頭辞が消える。該当テストの期待値を更新する。`ObjectFieldMismatch` の reason と `SubgroupTracker::check_object_after_fin` が返す reason 文字列は `MessageError` ではないため対象外とする。
- Properties の KVP 値長超過のエラー variant を `ProtocolViolation` に統一する。対象は `ObjectProperties::encode` / `TrackProperties::encode` / `LocProperties::encode` で、decode と `MessageParameters` / `SetupOptions` の既存動作に揃える。`ControlMessage::encode` のメッセージペイロード長 (16-bit Length) は別条件のため `PayloadTooLong` のままとする。
- `MessageError::MalformedTrack` の doc を §10.7 の入れ子 IMMUTABLE_PROPERTIES 条件を含むように更新する。
- 分類を検証するテストを追加・更新する。既存の `tests/test_object_properties.rs` の `nested_immutable_properties_rejected_on_decode`、`tests/test_track_properties.rs` の `encode_rejects_bytes_value_over_65535`、`tests/test_session/data_stream.rs` の `subgroup_object_with_malformed_properties_kvp_terminates_subscription` / `datagram_with_malformed_properties_kvp_terminates_subscription` / PRIOR_OBJECT_ID_GAP の reason 検査を更新し、`tests/test_error.rs` に `Display` と `reason()` のテストを追加する。`tests/test_message.rs` に `ControlMessage::encode` の `PayloadTooLong` が変わらないことを固定するテストを追加する。`tests/test_object_properties.rs` に `ObjectPropertyTracker::observe_object` の入れ子 IMMUTABLE_PROPERTIES が `MalformedTrack` になるテストと、encode の KVP 値長 65535 超が `ProtocolViolation` になるテストを追加する。`tests/test_video_decode_order.rs` に `prior_object_id_gap_of` の入れ子 IMMUTABLE_PROPERTIES が `MalformedTrack` になるテストを追加する。`tests/test_loc.rs` に encode の KVP 値長 65535 超が `ProtocolViolation` になるテストを追加する。

## 完了条件

- Object 側の入れ子 IMMUTABLE_PROPERTIES が `ObjectProperties::decode` で `MalformedTrack`、`ObjectProperties::encode` で `ProtocolViolation` になること。`ObjectProperties::decode_exact` 経由の `ObjectPropertyTracker::observe_object` と `prior_object_id_gap_of` も `MalformedTrack` を返すこと。`decode_exact` と `prior_object_id_gap_of` の doc が `MalformedTrack` を含むこと。
- Track 側の入れ子 IMMUTABLE_PROPERTIES が `TrackProperties::decode` / `encode` で `ProtocolViolation` のままであること。`decode_track_kv_pairs` の doc に §10.7 の malformed 条件が Object に限られることが明記されていること。`MessageError::MalformedTrack` の doc に §10.7 の入れ子 IMMUTABLE_PROPERTIES 条件が含まれていること。
- `MessageError` の `Display` に二重接頭辞が無いこと。`MessageError::MalformedTrack` と `validate_fetch_object` の `ProtocolViolation` の生成メッセージに "malformed track: " が含まれないこと。`MessageError::reason()` の戻り値が接頭辞なしであること。
- `ObjectProperties::encode` / `TrackProperties::encode` / `LocProperties::encode` の KVP 値長 65535 超が `ProtocolViolation` になること。`ObjectProperties::encode` / `TrackProperties::encode` / `LocProperties::encode` の `# Errors` が `ProtocolViolation` に更新されていること。`ControlMessage::encode` の `PayloadTooLong` は変わらないこと。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

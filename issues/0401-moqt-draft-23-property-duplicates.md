# Property の多重可否を型の定義に従って検証する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §8.4、§10.7、§12.1 条件 9、§16.8](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/object_properties.rs` の `ObjectProperties::encode` / `decode_kv_pairs` と `src/track_properties.rs` の `TrackProperties::encode` / `decode_track_kv_pairs` は、同一 Type を未知型も含めて一律拒否する。
受信時の重複も主に ProtocolViolation として返す。
`src/loc.rs` の `LocProperties::encode` / `decode` も未知 ID を含む重複を一律に拒否する。`crates/moq-sub/src/pipeline.rs` の Properties 抽出処理は LOC として再 decode し、その失敗をセッション終了へ渡す。

## 設計方針

対応する Property は定義上許可される場合を除いて単一とし、多すぎる受信を MalformedTrack とする。
未知 Property の複数インスタンスは保存して受理する。
IMMUTABLE_PROPERTIES 内外の検証と、複数を許す既知型がある場合の上書き・保持規則をそれぞれの定義に合わせる。
AUTHORIZATION_TOKEN など別の名前空間の多重規則を流用しない。
0400 の構造 decode と意味検証の分離を多重検証にも適用する。制御メッセージの decode は重複を保持して `Session` へ渡し、Request ID / Full Track Name を得た受信ハンドラで多重可否を検証する。違反は同じ Track の subscription / FETCH の cancel・通知へ渡し、初回 PUBLISH の文脈も保持する。Object 側も `decode_exact` を経由して `MalformedTrack` が失われないようにする。ローカル encode の不正は入力検証エラーとして扱う。
`LocProperties` の encode / decode も型別の多重規則へ変更し、未知型の複数インスタンスを保持する。
利用側の `extract_video_config` / `extract_audio_config` / `extract_timestamp_timescale` などによる再 decode でも正当な未知重複を拒否しない。
既知型の多重違反は `MalformedTrack` とし、request / stream の文脈を持つ受信処理へ伝えて取消・通知する。
Properties 抽出の失敗を一律に `request_session_termination` へ渡さず、構造エラーと malformed Track を区別する。

## 完了条件

- 未知 Type の同値・異値の複数インスタンスを lossless に往復できること。
- 単一に限られる既知 Type の重複は MalformedTrack として報告されること。
- immutable 内部の未知重複を受理し、受信した immutable serialization を書き換えないこと。
- 制御メッセージの wire 入力と直接 Session API の双方で、重複違反を Track 文脈付きの取消へ渡すこと。通常の映像・音声配送でも LOC の再 decode が未知重複を受理し、既知型の多重違反をセッション終了へ変換しないこと。

## 依存・関連

- 先行: [0400](0400-moqt-draft-23-property-scope.md)

## 解決方法

- 多重可否の表を `src/property_scope.rs` に集約した。draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) の
  "A Track or Object MUST NOT contain more than one instance of the Property."（Property の定義が複数を許す場合を除く）と
  §12.1 (Malformed Tracks) 条件 9 を根拠に、`allows_multiple_instances` を追加した。
  この実装が意味を理解する既知型 (§16.8 (Properties) Table 15 の 9 型と、Table 16 のうち `src/loc.rs` が解釈する 6 型) は単一、
  未知型と GREASE 値は複数を許す。既知型のいずれの定義節にも複数インスタンスを許す規則が無いため、draft 23 に複数を許す既知型は存在せず、
  上書き・保持の規則を型ごとに分ける必要はない。既知型に複数を許す定義が加わった場合に `allows_multiple_instances` を型ごとの判定へ
  変える必要があることを doc コメントに記した。
- `has_duplicate_single_property` を追加し、mutable リストと IMMUTABLE_PROPERTIES (0x0B) の内側を合わせた全インスタンスの型列から
  単一型の重複を検出する。draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の
  "If a Property allows multiple values, the same Property Type MAY appear in both the mutable list and inside Immutable Properties"
  を根拠に、単一型が両方に現れた場合は 2 インスタンスと数える。未知型と GREASE 値は数えないため、未知型を大量に含む入力でも走査は 1 回で済む。
- AUTHORIZATION_TOKEN (draft-ietf-moq-transport-23 §8.9 (Authorization Token Compression)) などの message parameter は
  別の名前空間の多重規則であり、Property の多重可否には流用しないことをモジュール doc に明記した。
- `src/track_properties.rs` の `decode_track_kv_pairs` から delta==0 を重複として拒否する処理を外し、同一 Type をそのまま保持するようにした。
  `TrackProperties::validate_multiplicity` と、スコープ検証と多重検証をまとめる `TrackProperties::validate` を追加した。
  構造 decode は重複を保持するだけで、多重可否の判定は Request ID と Full Track Name を持つ受信ハンドラが行う（構造と意味の分離）。
  `encode` はローカル入力の検証として、単一型の重複（IMMUTABLE の内側との重複を含む）を `ProtocolViolation` で拒否する。
  未知型の重複は保持したまま書き出す。
- `src/session/subscription/recv.rs`（初回 PUBLISH / SUBSCRIBE_OK）、`src/session/fetch.rs`（FETCH_OK）、
  `src/session/track_status.rs`（TRACK_STATUS_OK）の受信ハンドラを `validate_scope` から `validate` へ切り替えた。
  多重違反は既存の malformed Track 経路へ渡り、同じ Track の subscription / FETCH の cancel と
  `RequestTerminated { reason: MalformedTrack }` になる。初回 PUBLISH は `terminate_malformed_track_for_publish` が
  受信メッセージの Request ID と Full Track Name から Track 文脈を解決する既存の扱いをそのまま使う。
- `src/object_properties.rs` の `decode_kv_pairs` を構造のみの decode にし（入れ子 IMMUTABLE_PROPERTIES の拒否は維持）、
  `ObjectProperties::validate` にスコープ検証（§8.4 / §12.1 条件 8、§3.7 (Mandatory to Understand Track Properties) の
  Mandatory Track Property）と多重検証（§12.1 条件 9）を集約した。`decode` は重複を保持した後に `validate` を呼ぶため、
  `decode_exact` は `MalformedTrack` を `ProtocolViolation` へ畳まず data plane の取消経路へ渡す。`encode` は `validate` の
  `MalformedTrack` を `ProtocolViolation` に写し、Mandatory Track Property の範囲もローカル入力の不正として拒否する。
  IMMUTABLE_PROPERTIES の値は生バイト列のまま保持し、再 encode でも書き換えない。
- `src/loc.rs` の `LocProperties` も同じ規則へ変えた。decode は未知 ID の重複を保持し、単一に限られる既知 ID の重複を
  `MalformedTrack` にする。encode は同じ入力検証を `ProtocolViolation` で行う。既知 ID の getter は単一前提のため変更していない。
- `crates/moq-sub/src/pipeline.rs` に `skip_object_on_properties_error` を追加し、Properties 抽出の失敗を分類した。
  `MalformedTrack` は draft-ietf-moq-transport-23 §8.4 / §12.1 条件 9 がセッション終了ではなく購読・fetch の cancel を求めるものであり、
  Session の data plane が同じ Object Properties を decode して既に cancel しているため、セッション終了を依頼せず Object を捨てる。
  音声配送は `true`（この Object だけ捨てて継続）、映像配送は `VideoDelivery::MalformedTrack`（stream の読み取りを打ち切る）で表す。
  書式違反は従来どおり `request_session_termination` へ渡す。
- `src/loc.rs` の getter と `extract_video_config` / `extract_audio_config` / `extract_timestamp_timescale` / `extract_video_keyframe` は、
  未知 ID の重複を保持する decode を通るため正当な未知重複を拒否しない。
- `pbt/src/common.rs` の Track Properties サンプラーを修正し、単一に限られる既知 Property が mutable リストと IMMUTABLE の内側の
  両方に現れないようにした。旧サンプラーは同じ 0x02 を両方に生成し得たため、encode のローカル検証追加後に正常系 PBT が失敗する入力を
  作っていた。
- 利用者向けドキュメント (README / docs / SKILL) の draft 23 への一括更新は 0404 の範囲とし、この issue では触れていない。

## 完了条件の検証

- 未知 Type の同値・異値の複数インスタンスを lossless に往復できること: `tests/test_track_properties.rs` の
  `property_multiplicity::unknown_duplicate_types_roundtrip_losslessly` と `tests/test_object_properties.rs` の同名テストが、
  未知の偶数型・奇数型を同値と異値で複数持ち、encode → decode と decode → encode → decode の両方で入力順のまま一致することを検証する。
  `src/loc.rs` の `unknown_duplicate_property_id_accepted_on_decode` / `unknown_duplicate_property_id_roundtrips_on_encode` が
  LOC でも同じ性質を固定する。PBT の `pbt/tests/prop_track_properties.rs` と `pbt/tests/prop_object_properties.rs` の
  `unknown_duplicate_types_roundtrip` が、ランダムな型・インスタンス数・値で往復と `validate` の受理を検証する。
- 単一に限られる既知 Type の重複は MalformedTrack として報告されること:
  `tests/test_track_properties.rs` の `property_multiplicity::validate_reports_duplicate_known_type_as_malformed_track` /
  `validate_reports_duplicate_known_type_inside_immutable` / `validate_reports_duplicate_across_mutable_and_immutable` /
  `validate_reports_duplicate_immutable_properties`、`tests/test_object_properties.rs` の
  `property_multiplicity::decode_reports_duplicate_known_type_as_malformed_track` /
  `duplicate_across_mutable_and_immutable_is_malformed` / `duplicate_immutable_properties_are_malformed` が `MalformedTrack` を、
  `encode_rejects_duplicate_known_type*` が `ProtocolViolation` を固定する。`src/property_scope.rs` の
  `known_properties_allow_single_instance_only` / `duplicate_detection_ignores_unknown_types` が多重可否の表と未知型の扱いを
  固定する。PBT の `known_duplicate_property_is_malformed` が wire 入力とローカル encode の双方を検証する。
- immutable 内部の未知重複を受理し、受信した immutable serialization を書き換えないこと:
  `tests/test_track_properties.rs` と `tests/test_object_properties.rs` の
  `property_multiplicity::unknown_duplicates_inside_immutable_are_accepted_and_bytes_are_kept` が、内側の未知型の重複を
  `validate` が受理し、`immutable_properties()` が受信した生バイト列と一致することを検証する。
  `tests/test_session/property_duplicates.rs` の `publish_with_duplicate_track_property_in_immutable_is_cancelled` /
  `subgroup_object_with_duplicate_property_in_immutable_terminates_subscription` が内側の既知型の重複を取消へ渡す。
- 制御メッセージの wire 入力と直接 Session API の双方で、重複違反を Track 文脈付きの取消へ渡すこと:
  `tests/test_session/property_duplicates.rs` の `publish_with_duplicate_track_property_is_not_registered_and_is_cancelled` /
  `publish_with_duplicate_track_property_in_immutable_is_cancelled` /
  `subscribe_ok_with_duplicate_track_property_cancels_subscription` / `fetch_ok_with_duplicate_track_property_cancels_fetch` /
  `track_status_ok_with_duplicate_track_property_cancels_track_status` が直接 Session API を、
  `publish_with_duplicate_track_property_on_wire_is_cancelled` /
  `subscribe_ok_with_duplicate_track_property_on_wire_cancels_subscription` /
  `fetch_ok_with_duplicate_track_property_on_wire_cancels_fetch` /
  `track_status_ok_with_duplicate_track_property_cancels_related_requests` が wire 入力を検証する。いずれも `Established` の維持、
  `STREAM_MALFORMED_TRACK` による STOP_SENDING → RESET_STREAM、`RequestTerminated { reason: MalformedTrack }`、
  購読・fetch の `Terminated` を確かめる。`publish_with_unknown_duplicate_property_is_accepted` が未知型の重複を購読として
  登録することを固定する。
- 通常の映像・音声配送でも LOC の再 decode が未知重複を受理し、既知型の多重違反をセッション終了へ変換しないこと:
  `crates/moq-sub/src/pipeline.rs` の `audio_delivery_does_not_request_session_termination_on_malformed_track` /
  `video_delivery_does_not_request_session_termination_on_malformed_track` が、既知 ID の重複で `request_session_termination` が
  呼ばれないこと（終了依頼チャネルが空のまま）を、`audio_delivery_accepts_unknown_duplicate_loc_properties` が未知 ID の重複を
  配送が受理することを検証する。data plane 側は `tests/test_session/property_duplicates.rs` の
  `subgroup_object_with_duplicate_property_terminates_subscription` / `datagram_with_duplicate_property_terminates_subscription` /
  `subgroup_object_with_unknown_duplicate_property_is_accepted` が担う。
- 検証コマンド: `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo test --workspace` / `RUSTDOCFLAGS="-D warnings" cargo doc -p shiguredo_moqt --no-deps` がすべて成功する。

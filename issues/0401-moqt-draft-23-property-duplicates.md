# Property の多重可否を型の定義に従って検証する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-property-duplicates
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

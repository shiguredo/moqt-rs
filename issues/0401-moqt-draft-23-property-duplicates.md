# Property の多重可否を型の定義に従って検証する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-property-duplicates
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §8.4、§12.1 条件 9](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/object_properties.rs` の `ObjectProperties::encode` / `decode_kv_pairs` と `src/track_properties.rs` の `TrackProperties::encode` / `decode_track_kv_pairs` は、同一 Type を未知型も含めて一律拒否する。
受信時の重複も主に ProtocolViolation として返す。

## 設計方針

対応する Property は定義上許可される場合を除いて単一とし、多すぎる受信を MalformedTrack とする。
未知 Property の複数インスタンスは保存して受理する。
IMMUTABLE_PROPERTIES 内外の検証と、複数を許す既知型がある場合の上書き・保持規則をそれぞれの定義に合わせる。
AUTHORIZATION_TOKEN など別の名前空間の多重規則を流用しない。

## 完了条件

- 未知 Type の同値・異値の複数インスタンスを lossless に往復できること。
- 単一に限られる既知 Type の重複は MalformedTrack として報告されること。
- immutable 内部の未知重複を受理し、受信した immutable serialization を書き換えないこと。

## 依存・関連

- 関連: [0400](0400-moqt-draft-23-property-scope.md)

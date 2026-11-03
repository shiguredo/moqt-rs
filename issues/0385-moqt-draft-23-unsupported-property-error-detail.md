# UNSUPPORTED_EXTENSION に未対応 Property の詳細を付ける

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/add-moqt-draft-23-unsupported-property-error-detail
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §3.7、§9.4.2.2](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/track_properties.rs` の `TrackProperties::has_unknown_mandatory` は bool のみを返す。
`src/session/subscription/recv.rs` と `src/session/fetch.rs` の未知 Mandatory Track Property に対する拒否処理は、相手へ未対応の Property Type を通知する表現を持たない。

## 設計方針

Error Details 対応を前提に、Type `0x02` の UNSUPPORTED_PROPERTY を扱う。
UNSUPPORTED_EXTENSION の拒否時に未知 Mandatory Track Property の型を列挙して通知できるようにする。
複数 Detail を許し、IMMUTABLE_PROPERTIES 内の対象も調べ、GREASE は対象にしない。
任意の上流転送やリレー機能の追加は対象にしない。

## 完了条件

- 複数の未知 Mandatory Track Property について、それぞれ Type 値を持つ Detail を生成・受信できること。
- GREASE と既知 Property を未対応として報告しないこと。
- UNSUPPORTED_EXTENSION 以外に届いた Detail を、Error Details の適用規則どおり無視すること。

## 依存・関連

- 先行: [0384](0384-moqt-draft-23-request-error-details.md)
- 関連: [0402](0402-moqt-draft-23-track-status-mandatory-properties.md)

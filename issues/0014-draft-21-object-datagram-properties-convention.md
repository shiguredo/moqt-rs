# send_object_datagram と ObjectDatagram の properties_data 規約を統一する

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/change-object-datagram-properties-convention
- Polished: 2026-09-10

## 目的

Object Datagram の Object Properties を、本ライブラリの他の properties ブロックと同じ `Properties Length varint + Properties` の raw バイト列規約に統一する。現状は `ObjectDatagram` だけが Length 抜きで、example がワイヤ上に Length を 2 回書いてしまう。

## 現状

- `ObjectDatagram.properties_data` (`src/stream/datagram.rs`) だけが Length 抜き規約である。`encode` が `varint::encode(data.len())` を前置し、`decode` は Length を除去して返す。
- 他はすべて Length 込みである。`SubgroupObject::decode` / `FetchStreamObject::decode` は Properties Length を含む raw スライスを返し、`SubgroupObject::encode` / `FetchStreamObject::encode` は受け取った raw バイト列をそのまま書く。`ObjectFilterInput.properties_bytes` (`src/session/subscription/validation.rs`) と
  `send_subgroup_object` の `properties_bytes` も Length 込みを要求する。`ObjectProperties::encode` と `LocProperties::encode` は Length 込みを返す。
- `examples/moqt-publisher/src/datagram_writer.rs` は `LocProperties::encode()` の出力を `ObjectDatagram.properties_data` に入れ、`ObjectDatagram::encode` がさらに Length を前置するため、ワイヤは `Properties Length | Properties Length | Key-Value-Pairs...` になる。受信側は `ObjectProperties::decode` の失敗で
  Malformed Track として subscription を終端する。
- `recv_object_datagram` は Length 抜きの `properties_data` に Length を付け直して `ObjectFilterInput.properties_bytes` を作っている。
- `pbt/tests/prop_stream/main.rs` のコメントが「`ObjectDatagram` の `properties_data` は Length を含まない」という例外規約を明記しており、PBT もその前提で任意バイト列を渡している。

根拠 (draft-ietf-moq-transport-21 §11.1.3 (Object Properties)):

> "Object Properties ... serialized as a length in bytes followed by Key-Value-Pairs"

## 設計方針

- `ObjectDatagram.properties_data` を Length 込みの raw バイト列に統一する (`SubgroupObject` / `FetchStreamObject` と同じ規約)。
- `ObjectDatagram::encode` は `properties_data` をそのまま書く。先頭の Properties Length varint を読み、次を検証して違反は `ProtocolViolation` を返す。
  - 空スライス (Properties Length varint すら含まない) を拒否する
  - Properties Length = 0 を拒否する (datagram では禁止。受信側は §11.2.1 で PROTOCOL_VIOLATION)
  - 宣言 Length と実データ長の一致を検証する (不一致のワイヤを生成しない)
- `ObjectDatagram::decode` は Properties Length を含む raw スライスを返す。Length = 0 の拒否と境界検証は現行どおり維持する。
- `recv_object_datagram` は `properties_data` に Length を付け直す処理を削除し、そのまま `ObjectFilterInput.properties_bytes` に渡す。
- `send_object_datagram` は入力を Length 込みのまま変更しない。`data.is_empty()` に加えて Properties Length = 0 と宣言長不一致を拒否する (Session 段で早期に弾く既存方針に合わせる)。
- example (`datagram_writer.rs`) は `LocProperties::encode()` をそのまま渡す組み立てが正しくなるため変更不要。`pbt/tests/prop_stream/main.rs` の例外コメントを削除・更新する。
- 公開 API の後方互換のない変更 (`properties_data` の意味と `encode` / `decode` の出力が変わる) のため、ブランチは `feature/change-...`、`CHANGES.md` の `## develop` に `[CHANGE]` として記載する (0025 と同じ扱い)。
- 既存テストの追従:
  - `pbt/tests/prop_stream/datagram.rs` は length-less の任意バイト列を `properties_data` に入れているため、Length 込みの整合したブロックを生成する形に変更する。
  - `tests/test_session/data_stream.rs` の `malformed_track_via_datagram_terminates_subscription` は二重 Length による偶発的な decode 失敗に依存しているため、統一後に意図した PRIOR_OBJECT_ID_GAP 経路で Malformed Track になることを確認する。
  - length-less 規約を前提にした単体テスト (`ObjectDatagram` を直接構築するもの) を洗い出して更新する。
  - fuzz ターゲット (`fuzz/fuzz_targets/fuzz_roundtrip.rs` 等) の decode / encode 往復が通ることを確認する。
- 影響範囲: `src/stream/datagram.rs`、`src/session/data.rs`、`pbt/tests/prop_stream/datagram.rs` と `main.rs`、`tests/test_session/data_stream.rs` ほか datagram 関連テスト、`skills/shiguredo-moqt/SKILL.md`。

## 完了条件

- `ObjectDatagram.properties_data` が Length 込みに統一され、`ObjectDatagram::encode` / `decode` / `recv_object_datagram` / `send_object_datagram` の規約が一致すること
- `ObjectDatagram::encode` と `send_object_datagram` が Properties Length = 0 と宣言 Length と実データ長の不一致を拒否すること
- `datagram_writer.rs` と同じ組み立て (`LocProperties::encode()` → `ObjectDatagram` → `encode` → 受信側 `recv_datagram`) を再現したテストで、受信が受理され Malformed Track にならないこと
- `pbt/tests/prop_stream/datagram.rs` ほか length-less 前提のテストが新規約に追従し、`cargo test --workspace` と PBT が通ること
- `CHANGES.md` の `## develop` に `[CHANGE]` として記載されていること

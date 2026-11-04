# Low Overhead Container (LOC)

[draft-ietf-moq-loc-04](https://datatracker.ietf.org/doc/html/draft-ietf-moq-loc-04) の実装状況です。

## LOC Properties

- Timestamp (`0x10`)
- Timescale (`0x08`)
- Video Config (`0x0D`)
- Video Frame Marking (`0x09`、1-4 bytes)
- Audio Level (`0x0C`、0-255)
- Audio Config (`0x0F`)

偶数 ID は varint 値、奇数 ID は長さ付きバイト列として encode / decode します。
Public / Private の配置はアプリケーション層の責務です。

`IMMUTABLE_PROPERTIES` (`0x0B`) の内側も、既知プロパティの accessor と
`find_varint` / `find_bytes` が探索します (draft-ietf-moq-transport-23 §10.7
(Immutable Properties) の "MUST search both")。`iter` / `len` / `is_empty` は最上位リストだけを
対象にし、`IMMUTABLE_PROPERTIES` の内側は含みません。

## 未対応

- §2.1 (Video Payload Format)：annexB / length prefix / parameter set を解釈しない (example は AVCC 形式と Video Config の経路のみ対応する)
- §3 (Payload Encryption)：Secure Objects 連携による暗号化と復号は未実装 (MSF 側の signaling フィールドは扱う)
- `IMMUTABLE_PROPERTIES` の内側だけにある LOC 固有の値域・長さ違反は `MalformedKeyValue` として通知しない (docs/missing-features.md §4.1)

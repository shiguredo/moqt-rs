# Low Overhead Container (LOC)

[draft-ietf-moq-loc-05](https://datatracker.ietf.org/doc/html/draft-ietf-moq-loc-05) の実装状況です。

## LOC Properties

- Timestamp (`0x10`)
- Timescale (`0x08`)
- Codec String (`0x11`、UTF-8)
- Video Config (`0x0D`)
- Video Frame Marking (`0x09`、1-4 bytes)
- Audio Level (`0x0C`、0-255)
- Audio Config (`0x0F`)

偶数 ID は varint 値、奇数 ID は長さ付きバイト列として encode / decode します。
Public / Private の配置はアプリケーション層の責務です。

`Codec String` は WebCodecs Codec Registry の codec 値の UTF-8 文字列です
(draft-ietf-moq-loc-05 §2.3.1.3)。`codec_string` は欠落を `Ok(None)`、不正な UTF-8 を
`MalformedKeyValue` で返します。内側の `IMMUTABLE_PROPERTIES` の KVP が解析・検証できない
場合は `Ok(None)` になります (既知の制約)。encode / decode は最上位の `0x11` の UTF-8 を検証し、
`IMMUTABLE_PROPERTIES` の内側の `0x11` も検証します。encode はさらに内側の構造・値域も
検証するため、解析できない内側を持つ入力は拒否します。decode は内側 KVP が解析できる場合
だけ内側の `0x11` を検証し、解析できない内側は受理します (既知の制約)
(draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の `MALFORMED_KEY_VALUE`)。

`IMMUTABLE_PROPERTIES` (`0x0B`) の内側も、既知プロパティの accessor と
`find_varint` / `find_bytes` が探索します (draft-ietf-moq-transport-23 §10.7
(Immutable Properties) の "MUST search both")。`iter` / `len` / `is_empty` は最上位リストだけを
対象にし、`IMMUTABLE_PROPERTIES` の内側は含みません。

## 未対応

- §2.1 (Video Payload Format)：annexB / length prefix / parameter set を解釈しない (example は AVCC 形式と Video Config の経路のみ対応する)
- §3 (Payload Encryption)：Secure Objects 連携による暗号化と復号は未実装 (MSF 側の signaling フィールドは扱う)
- `IMMUTABLE_PROPERTIES` の内側だけにある Audio Level の値域違反と Video Frame Marking の長さ違反は `MalformedKeyValue` として通知しない (docs/missing-features.md §4.1)。`Codec String` の不正 UTF-8 は、内側 KVP が解析できる場合に限り decode が拒否する (encode は解析できない内側を拒否する)

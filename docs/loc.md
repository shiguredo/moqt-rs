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
場合、accessor の探索結果は `Ok(None)` になります。encode / decode は最上位の `0x11` の
UTF-8 を検証し、`IMMUTABLE_PROPERTIES` の内側の `0x11` も検証します。encode と decode は
さらに内側の構造・値域・重複・入れ子の `0x0B` も検証するため、解析できない内側を持つ入力は
拒否します
(draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の `MALFORMED_KEY_VALUE`)。

`IMMUTABLE_PROPERTIES` (`0x0B`) の内側も、既知プロパティの accessor と
`find_varint` / `find_bytes` が探索します (draft-ietf-moq-transport-23 §10.7
(Immutable Properties) の "MUST search both")。`iter` / `len` / `is_empty` は最上位リストだけを
対象にし、`IMMUTABLE_PROPERTIES` の内側は含みません。

## Video Payload Format

`loc::video_payload` は §2.1.3 (Length Prefixes in Payload) / §2.1.4 (Start Code Prefixes in
Payload) の NAL Unit 境界処理を提供します。

- `NalUnitFormat::length_prefix(width)` は 1〜4 バイトの length prefix を表します
- `NalUnitFormat::start_code(width)` は 3 / 4 バイトの start code を表します
- `NalUnitFormat::from_length_size_minus_one(value)` は Video Config の
  `lengthSizeMinusOne` (0〜3) を 1〜4 バイトの length prefix に写像します
- `NalUnitDecoder` は payload を NAL Unit の借用スライスへ分割します
- `encode_nal_unit` は NAL Unit を length prefix / start code 付きで追記します
- §2.1.3 の「length prefix の値 1 は start code と解釈する」を実装し、3 / 4 バイト prefix の
  値 1 は対応する start code へ切り替えます。1 / 2 バイト prefix の値 1 と、1 バイトの
  NAL Unit を length prefix で表す入力は曖昧なため拒否します

parameter set の内容 (SPS / PPS / VPS) や、payload 内 / Video Config のどちらに置くかは
コーデック層の責務であり、この API では解釈しません。

## 未対応

- §2.1 (Video Payload Format)：境界処理の API は追加したが、example の H.264 / H.265
  パイプラインは AVCC 4 バイト length prefix + Video Config の経路だけを使っている。
  annexB の payload 内 parameter set と 3 バイト start code の example 統合は未対応
- §3 (Payload Encryption)：Secure Objects 連携による暗号化と復号は未実装 (MSF 側の signaling フィールドは扱う)
- `LocProperties::decode` は `IMMUTABLE_PROPERTIES` の内側の Audio Level の値域違反と Video Frame Marking の長さ違反を `MalformedKeyValue` で拒否する。Session の受信経路も `ObjectProperties::decode` の同じ検証を経由し、MALFORMED_KEY_VALUE によるセッション終了へ渡す

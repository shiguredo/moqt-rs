# 同一 Group の FETCH Object ID Delta に 2 を加える

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-fetch-object-id-delta
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §11.4.1.1](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/stream/decoder.rs` の `FetchStreamDecoder::resolve_object` は同一 Group で明示された Object ID Delta を prior にそのまま足す。
`src/stream/encoder.rs` の `FetchStreamEncoder::encode_object` も実際の ID 差をそのまま送る。

## 設計方針

同一 Group で delta が present の場合の復元を prior + delta + 2 にし、encoder は差から 2 を引く。
連続する ID の +1 はフィールド省略を使う。
最初の Object と Group ID Delta が present のときの絶対 ID、範囲指示の +1 規則を混同しない。

## 完了条件

- prior 0 → ID 1 は省略、prior 0 → ID 2 は delta 0 になる golden バイト列を双方向で検証すること。
- Group 変更、Subgroup 変更、範囲指示の後の Object、u64 境界の加算 overflow を検証すること。
- 自己 roundtrip だけでなく draft 23 の独立した既知バイト列で旧実装との差を検出すること。

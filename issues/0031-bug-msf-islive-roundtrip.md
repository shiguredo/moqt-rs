# MSF の isLive=false で targetLatency / buffers の encode と decode を対称にする

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-islive-roundtrip

## 目的

draft-ietf-moq-msf-01 §5.2.8 / §5.2.9 に従い、`isLive=false` のとき `targetLatency` / `buffers` を無視する。encode と decode の往復を一致させる。

## 現状

`src/msf.rs` の `DisplayJson for MsfTrack` は `is_live` を見ずに `targetLatency` / `buffers` を出力する。一方 `decode_track` は `isLive=false` で `(None, None)` に正規化する。このため手組みの `MsfTrack { is_live: false, target_latency: Some(500) }` は `decode(encode(x)) != x` になる。

`pbt/tests/prop_msf.rs` のコメントは「エンコード側では出力されない」と書くが事実に反する。

根拠 (draft-ietf-moq-msf-01 §5.2.8 / §5.2.9): "If isLive is FALSE, this field MUST be ignored."

## 設計方針

encode 時に `is_live == false` なら `targetLatency` / `buffers` を出力しない。decode と対称化する。PBT のコメントも実態に合わせて修正する。

## 完了条件

- `isLive=false` のとき encode が `targetLatency` / `buffers` を出力しないこと
- `decode(encode(x))` が仕様上有意な値で一致すること
- PBT のコメントと生成器が実態に一致すること

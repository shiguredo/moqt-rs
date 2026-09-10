# MSF の isLive=false で targetLatency / buffers の encode と decode を対称にする

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-islive-roundtrip
- Polished: 2026-09-10

## 目的

draft-ietf-moq-msf-01 §5.2.8 / §5.2.9 は `isLive=false` のとき `targetLatency` / `buffers` を無視する（受信側の規則）。デコーダは既に無視して `None` に正規化しているのに、エンコーダは `isLive=false` でも値を出力しており非対称である。エンコーダをデコーダの正規化に合わせ、正規形の値で往復が一致するようにする。

## 現状

- `src/msf.rs` の `impl DisplayJson for MsfTrack` は `is_live` を見ずに `targetLatency` / `buffers` を出力する。
- `impl DisplayJson for MsfCloneTrack` も同様に、`is_live == Some(false)` でも `targetLatency` / `buffers` を出力する。
- 一方 `decode_track` は `is_live == false` で `(None, None)` に、`decode_clone_track` は `is_live == Some(false)` で `(None, None)` に正規化する。
- `MsfCloneTrack` の `is_live == None` は親からの継承を意味するため、この場合は出力を維持する必要がある（`tests/test_msf/error_cases.rs` の `clone_omitted_kept` が固定している）。
- `pbt/tests/prop_msf.rs` のコメントは「エンコード側では出力されない」と書くが、現状のエンコーダは出力する。生成器は `!is_live` のとき自分で `None` に正規化しており、往復は正規形でのみ成立している。

根拠 (draft-ietf-moq-msf-01):

- §5.2.8 (Target latency): "If isLive is FALSE, this field MUST be ignored."
- §5.2.9 (Buffers): "If isLive is FALSE, this target buffer property MUST be ignored."

これらは受信側の「無視」規則であり、エンコーダの出力を直接禁止する MUST ではない。本 issue は、デコーダが無視する値をエンコーダが出力しないようにする実装規約を揃えるものとして位置付ける。

## 設計方針

- `MsfTrack`: `is_live == false` のとき `targetLatency` / `buffers` を出力しない。
- `MsfCloneTrack`: `is_live == Some(false)` のとき `targetLatency` / `buffers` を出力しない。`is_live == None`（親から継承）と `is_live == Some(true)` では出力する。
- `decode_track` / `decode_clone_track` の正規化は変更しない。非正規な値（`isLive=false` かつ `Some`）は仕様上無意なため、`decode(encode(x))` の一致は生成器が作る正規形でのみ成立することを明記する。
- `pbt/tests/prop_msf.rs` の生成器の `!is_live` 正規化は往復維持のため現状のまま残し、コメントをコード修正後の実態に合わせる。
- 「`isLive=false` のとき encoded JSON に `targetLatency` / `buffers` の key が現れないこと」を単体テストで追加する（`tests/test_msf/encode_decode_roundtrip.rs` の `is_complete_not_emitted_when_false` と同型）。

## 完了条件

- `isLive=false` の `MsfTrack` の encode が `targetLatency` / `buffers` を出力しないこと
- `isLive` が `Some(false)` の `MsfCloneTrack` の encode が `targetLatency` / `buffers` を出力せず、`None` / `Some(true)` では出力すること
- 生成器が作る正規形（`isLive=false` なら `targetLatency` / `buffers` は `None`）で `decode(encode(x)) == x` が成立すること
- `isLive=false` の encoded JSON に該当 key が現れないことを検証する単体テストが `tests/test_msf/` に追加されていること
- PBT のコメントと生成器がコード修正後の実態に一致すること
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加すること

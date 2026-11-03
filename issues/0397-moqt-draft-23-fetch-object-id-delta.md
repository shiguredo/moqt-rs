# 同一 Group の FETCH Object ID Delta に 2 を加える

- Created: 2026-11-03
- Completed: 2026-11-03
- Branch: develop
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

## 解決方法

`src/stream/decoder.rs` の `FetchStreamDecoder::resolve_object` を draft-ietf-moq-transport-23 §11.4.1.1 (Flags) に合わせた。
Group ID Delta が present のときは Object ID Delta を新しい Group の絶対 Object ID とし、absent のときは `prior Object ID + Object ID Delta + 2` に解決する。
Object ID Delta が absent なら Group に関係なく `prior Object ID + 1` とする。
present / absent の判定は解決後の Group ID の比較ではなく `FetchStreamObject::group_id` の有無で行い、節の条件をそのまま写した。
加算は `checked_add` を 2 段掛け、u64 を超える場合は DELTA_ENCODING_OVERFLOW (0x400) を返す。

`src/stream/encoder.rs` の `FetchStreamEncoder::encode_object` は、同一 Group の絶対 Object ID の差から 2 を引いた値を Object ID Delta として送る。
差が 1 のときは Object ID Delta を省略し、Group が変わるときは Group ID Delta を present にして絶対 Object ID を送る。
同一 Group では「直前より Object ID が大きいこと」を先に検査しているため、差が 1 なら省略、2 以上なら差 - 2 が必ず成立する (`checked_sub(2)` は防御)。

End of Range の同一 Group の +1 と、End of Range の Group ID Delta に +1 が付かない規則 (§11.4.2.1 (ID Decoding)) は変更せず、Object の +2 とは別の関数 (`resolve_end_of_range` / `end_of_range_deltas`) に分けたままにした。
`src/stream/fetch.rs` のモジュール doc と `FetchStreamObject::group_id` / `object_id` の doc を draft 23 の規則に合わせて更新した。

テストは `tests/test_stream/fetch_object_id_delta.rs` に golden バイト列を追加し、デコードとエンコードの両方向で固定した。
あわせて `pbt/tests/prop_stream/encoder.rs` に、エンコーダの出力を低水準 codec (`FetchStreamEntry::decode`) で読み直し、Object ID Delta の生値が仕様の規則に一致することを検証する PBT を追加した。
高水準デコーダを経由したラウンドトリップはエンコーダとデコーダが同じ誤りを共有していても成立するため、wire の生値を独立に検証する層を足している。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| prior 0 → ID 1 は省略、prior 0 → ID 2 は delta 0 を双方向で検証 | `prior_plus_one_omits_object_id_delta` が `0x01 0x00`、`prior_plus_two_encodes_object_id_delta_zero` が `0x05 0x00 0x00` をデコードとエンコードの両方向で検証する |
| Group 変更 | 昇順 golden の Object 5 (Group 10 → 12、Group ID Delta 1) と降順 golden の Object 4 (Group ID Delta 0 でも present なら絶対 Object ID) を `..._golden_bytes_decode` / `..._golden_bytes_encode` で検証する |
| Subgroup 変更 | 昇順 golden の Object 6 (Subgroup 3 → 7、Object 0 → 2 を delta 0 で表現) と `test_subgroup_change_with_object_id_delta` / `test_subgroup_change_with_object_id_absent` |
| 範囲指示の後の Object | `object_after_end_of_range_uses_plus_two_rule` が End of Range の +1 (10 → 20 を delta 9) と End of Range 後の Object の +2 (20 → 31 を delta 9) を同じストリームで検証する |
| u64 境界の加算 overflow | `same_group_object_id_delta_reaches_u64_max` が u64::MAX ちょうどの受理を、`same_group_object_id_delta_plus_two_overflow_is_delta_encoding_overflow` が u64::MAX + 0 + 2 の DELTA_ENCODING_OVERFLOW を検証する |
| 自己 roundtrip 以外の独立した既知バイト列で旧実装との差を検出 | golden は仕様から手計算したバイト列であり、旧実装 (+1) ではデコードとエンコードの両方で値が変わる (詳細は下記) |

golden の `0x05 0x00 0x00` は、旧実装のデコーダでは prior + 0 = prior に解決されて単調増加検査に違反し、
旧実装のエンコーダは prior 0 → Object ID 2 に対して delta 2 の `0x05 0x02 0x00` を書き出す。
そのため同じ golden バイト列がデコードとエンコードの両方向で旧実装との差分を検出する。
実際に `src/stream/decoder.rs` と `src/stream/encoder.rs` を draft 22 相当の実装へ戻して新テストを実行すると、
`prior_plus_one_omits_object_id_delta` 以外の 8 件が失敗し、PBT の `object_id_delta_matches_draft_23_rule` も失敗することを確認した。
`object_id_delta_matches_draft_23_rule` は低水準 codec で wire の生値を独立に検証する。

検証コマンドは `cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace` を通した。

# FetchStreamEncoder と FetchStreamDecoder の Publisher Priority 検証を対称にする

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-fetch-priority-validation-symmetry
- Polished: {YYYY-MM-DD}

## 目的

`FetchStreamEncoder::encode_object` が生成を許す FETCH 応答を、自身の `FetchStreamDecoder` (および draft 準拠の receiver) が Malformed Track として拒否する状態をなくす。draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 1 の判定を encoder 側でも同じ粒度で行う。

## 現状

- draft-ietf-moq-transport-21 §12.1 (Malformed Tracks) 条件 1:

  > An Object with a particular Subgroup ID is received, but its Publisher Priority is different from that of the previous Object with the same Subgroup ID.

- `src/stream/decoder.rs` の `FetchStreamDecoder::validate_fetch_object` は `FetchValidationState::subgroup_priorities` に `(group_id, subgroup_id)` ごとの最後の Publisher Priority を記録し、同じ key の Object が異なる Priority を持つと `ProtocolViolation` を返す。datagram 起源 Object は Subgroup ID を持たないため記録対象から除外する。
- `src/stream/encoder.rs` の `FetchStreamEncoder::encode_object` は直前の 1 Object (`prior_state`) とだけ比較し、同一 group・同一 subgroup かつ双方が非 datagram 起源のときだけ Priority 変更を拒否する。
- この差により、間に別 subgroup の Object や datagram 起源 Object が挟まると encoder は Priority 変更を検出できない。一時テストで次の 2 例を確認した (encoder はすべて `Ok(())`、同じバイト列の decoder は 3 件目で `"malformed track: publisher priority changed within the same subgroup in a FETCH response"`)。
  - group 0: subgroup 0 (Priority 128) → subgroup 1 (Priority 64) → subgroup 0 (Priority 32)
  - group 0: subgroup 0 (Priority 128) → datagram 起源 (Priority 64) → subgroup 0 (Priority 32)
- `FetchStreamEncoder` は `src/session` からは使われておらず、公開 API の利用者が直接使う。encoder の出力が decoder で拒否されるため、利用者は不正な FETCH 応答を生成しうる。

## 設計方針

- draft §12.1 条件 1 の「same Subgroup ID の previous Object」に合わせ、encoder も `(group_id, subgroup_id)` ごとの最後の Publisher Priority を追跡して比較する。
- datagram 起源 Object は Subgroup ID を持たないため、比較にも記録にも使わない (decoder と同じ扱い)。
- group が前進したときは過去 group の記録を破棄し、記録量が Object 数に比例して増えないようにする。
- decoder の検証内容は変えない。

## 完了条件

- 別 subgroup の Object を挟んだ同一 subgroup の Priority 変更を `encode_object` が `ProtocolViolation` で拒否すること
- datagram 起源 Object を挟んだ同一 subgroup の Priority 変更を `encode_object` が `ProtocolViolation` で拒否すること
- 同一 subgroup で同一 Priority の連続、subgroup 変更直後の異なる Priority、datagram 起源 Object のみの列などの正常系が維持されること
- `FetchStreamDecoder` の既存挙動とテストが変わらないこと
- encoder / decoder の対称性を検証するテストが追加されていること

# GREASE 値が Mandatory Track Property 範囲に入る場合に malformed としない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-grease-property-mandatory-range
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §16.8 (Properties) の Table 14 は GREASE の Property Type を `0x7f * N + 0x9D`、Scope を Any として予約している。§13 (Grease) は未知値の受信でセッションを閉じることを MUST NOT とし、未知 Property は §8.4 (Track and Object Properties) に従うとする。

> Because new values in these registries can be defined without negotiation, implementations MUST handle unknown values gracefully. Endpoints MUST NOT close the session solely because they received an unknown value.

一方 §3.6 (Mandatory Track Properties) は 0x4000-0x7FFF を Mandatory Track Property とし、Object Property として現れたら malformed とする。

> Property types in the range 0x4000-0x7FFF are designated as Mandatory Track Properties. These properties MUST have Track scope.

(中間の "Mandatory Track Properties have special handling rules ..." の段落は省略)

> An Object received with a Mandatory Track Property as an Object Property is malformed (see Section 12.1).

GREASE 値の一部は 0x4000-0x7FFF に入るため、現状は GREASE の Property を Object Property として送る peer の Object が malformed track になり、購読を打ち切る (§12.1 (Malformed Tracks) の "When a subscriber detects a Malformed Track, it MUST cancel any corresponding subscription or fetches for that Track from that publisher")。

## 現状

- `src/object_properties.rs` の `decode_kv_pairs` は `src/track_properties.rs` の `MANDATORY_TRACK_PROPERTY_MIN` (0x4000) から `MANDATORY_TRACK_PROPERTY_MAX` (0x7FFF) までを無条件に `ProtocolViolation("malformed track: mandatory property in object scope")` にする。GREASE 値の判定は行っていない
- `src/grease.rs` の `is_grease` は公開されているが、`src/` 内からは参照されていない (`tests/test_grease.rs` と `pbt/tests/prop_grease.rs` が単体で検証しているのみ)。この範囲判定でも使われていない
- `0x7f * N + 0x9D` が 0x4000-0x7FFF に入るのは N = 128 (0x401D) から N = 255 (0x7F1E) までの 128 値である。N = 256 は 0x7F9D で範囲外になる
- GREASE 値は型の偶奇が交互になる (`0x9D` が奇数、`0x7f * N` の偶奇が N に依存するため)。N = 128 の 0x401D は奇数型 (長さ付きバイト列)、N = 129 の 0x409C は偶数型 (varint) である
- 0x4000-0x7FFF の Property を Object Property として与えるテストは存在せず、この分岐はテストで固定されていない
- 同じ衝突は Track scope にもある。`src/track_properties.rs` の `TrackProperties::has_unknown_mandatory` は 0x4000-0x7FFF を一律に未知の必須プロパティとして扱う。本 issue は Object scope の malformed 判定のみを対象とし、Track scope は対象外とする

## 設計方針

- 範囲判定から GREASE 値を除外し、malformed の条件を「0x4000-0x7FFF に入り、かつ GREASE 値でない」に変える。`src/grease.rs` の `is_grease` を使う
- GREASE 値は未知 Property として §8.4 と §16.8 に従い保持・転送する。値の読み飛ばしは型の偶奇の規則 (偶数型は varint、奇数型は長さ付きバイト列) に従う
- GREASE 値でない 0x4000-0x7FFF は現状どおり malformed とする (§3.6)
- draft 内部の矛盾と採用した解釈を明記する。§3.6 と §16.8 の登録ポリシーは 0x4000-0x7FFF を Mandatory Track Property 専用とし、Object scope での登録を禁じる。

  > 0x4000 to 0x7FFF: Reserved for Mandatory Track Properties (see Section 3.6). Properties registered in this range MUST have Track scope; Object scope properties MUST NOT be registered in this range.

  しかし GREASE 値は IANA に登録された Property ではなく、Table 14 が Scope Any として予約した値である。§3.6 の「Mandatory Track Property」は登録された必須トラックプロパティを指し、予約値である GREASE を含まないと解釈する。この解釈は §13 の MUST NOT (未知値の受信だけでセッションを閉じない) と §8.4 の未知 Property の転送 MUST に整合する
- この解釈は将来の draft 改版で変わりうる。draft 由来の規則であることをコードコメントに残す

## 完了条件

- GREASE 値 (奇数型の例: N = 128 の 0x401D、偶数型の例: N = 129 の 0x409C) を Object Property として含むバイト列の decode が成功し、値がそのまま保持されることを固定するテストが追加されていること
- 同じ Object を Session 経由で受信しても malformed track として購読が打ち切られないことを固定するテストが追加されていること
- GREASE 値でない 0x4000 と 0x7FFF を Object Property として受信すると、引き続き malformed track になることを固定するテストが追加されていること (非退行)
- Track scope の `TrackProperties::has_unknown_mandatory` の挙動を変えていないこと

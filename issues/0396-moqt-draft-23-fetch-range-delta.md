# FETCH の End of Range を delta 符号化する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-fetch-range-delta
- Polished: 2026-11-03

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §11.4.2.1、§11.4.2.2](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/stream/fetch.rs` の End of Range は Group ID / Object ID を直接持ち、`src/stream/decoder.rs` / `encoder.rs` の `update_prior_for_end_of_range` は受けた値を絶対 Location として保持する。
範囲指示自身の ID を prior から delta 復元する処理はない。

## 設計方針

FETCH_HEADER 直後の最初の entry が範囲指示なら絶対 ID とする。それ以外の範囲指示は、ストリーム内で最初に現れる範囲指示であっても、直前の Object または範囲指示を基準にする。
Group は Ascending なら加算、Descending なら減算する。
同一 Group の Object は prior + delta + 1、Group が変わる場合は絶対 Object ID にする。
直前の actual Object の Subgroup ID / Priority を範囲指示で消さず保持する。
低水準 codec の raw delta と高水準の絶対 Location の契約を明記する。

## 完了条件

- FETCH_HEADER 直後の範囲指示と、Object の後に初めて現れる範囲指示を区別し、連続する範囲指示、Object との交互配置、昇順・降順・同一 Group を golden バイト列で検証すること。
- 加算 overflow / 減算 underflow が DELTA_ENCODING_OVERFLOW になること。
- actual Object が未出現の prior Subgroup ID / Priority 参照を MALFORMED_OBJECT で拒否し、既出なら保持すること。

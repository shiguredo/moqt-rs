# GREASE 値が Mandatory Track Property 範囲に入る場合に unknown mandatory としない

- Created: 2026-09-21
- Completed: 2026-09-25
- Branch: feature/fix-grease-track-property-mandatory-range
- Polished: 2026-09-22

## 目的

draft-ietf-moq-transport-21 §16.8 (Properties) の Table 14 は GREASE 用に予約された Property Type を `0x7f * N + 0x9D` として Scope を Any と定め、§13 (Grease) は
「未知値を受けただけでセッションを閉じてはならない MUST NOT」と規定する。一方 §3.6 (Mandatory Track Properties) は 0x4000-0x7FFF を Mandatory Track Property とし、
理解できない Mandatory Track Property を受けた端末に「その Track を処理も転送もしてはならない」を課す。

GREASE 値のうち N = 128〜256 (0x401D〜0x7F9D の 129 値) はこの範囲に重なるため、現状は GREASE を送る peer の Track を購読できない (N = 257 は 0x801C で範囲外)。Object scope 側は [issues/0121](../issues/0121-bug-grease-property-mandatory-range.md) で扱う。本 issue は Track scope 側を扱う。

## 現状

- `src/track_properties.rs` の `TrackProperties::has_unknown_mandatory` は `MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX` の範囲判定だけを行い、既知の Mandatory Track Property 以外を unknown mandatory とみなす。`src/grease.rs` の `is_grease` による除外が無い
- 呼び出し元は `src/session/subscription/recv.rs` の `Session::handle_peer_publish` と `Session::handle_peer_subscribe_ok`、
  `src/session/fetch.rs` の `Session::handle_peer_fetch_ok` である。検出すると §3.6 に従って該当の購読 / fetch を終了する
  (cancel の手段は [issues/0110](../issues/0110-bug-cancel-request-on-mandatory-track-property.md) で扱う)
- GREASE 値の偶奇は N の偶奇で入れ替わる (0x401D は奇数、0x409C は偶数) ため、型の偶奇が交互になり、varint 値として解釈される GREASE 値と長さ付きバイト列として解釈される GREASE 値の両方が 0x4000-0x7FFF に入る。`TrackProperties::decode` はどちらも受理する
- GREASE 値 (0x401D〜0x7F9D) を与える Track Property のテストは無く、この分岐は未テストである (非 GREASE の 0x4000 / 0x5000 を与える既存テストはある)

## 設計方針

- `TrackProperties::has_unknown_mandatory` の判定で `src/grease.rs` の `is_grease` を除外し、GREASE 値は未知 Property として §8.4 どおり保持・転送する
- 同じ衝突は Object scope の `src/object_properties.rs` にもある。判定の実装を共有するか、少なくとも同じ解釈であることを doc コメントで示す ([issues/0121](../issues/0121-bug-grease-property-mandatory-range.md) と同時に対応するのが望ましい)
- draft 内部の矛盾 (§3.6 が範囲全体を Mandatory とするのに対し、Table 14 が GREASE の Scope を Any とする) を issue に記録し、採用した解釈の根拠 (§13 の MUST NOT と Table 14) を明示する

## 完了条件

- GREASE 値 (0x401D〜0x7F9D のいずれか。下限 0x401D と上限 0x7F9D の両方を含める) を Track Property として含む PUBLISH / SUBSCRIBE_OK / FETCH_OK を受信しても、unknown mandatory として扱わないことを固定するテストが追加されていること
- GREASE 以外の未知 Mandatory Track Property は従来どおり §3.6 の対象であり続けることを固定するテストが追加されていること
- 偶奇両方の GREASE 値 (varint 型と長さ付きバイト列型) を覆っていること
- Object scope 側 ([issues/0121](../issues/0121-bug-grease-property-mandatory-range.md)) と同じ解釈で実装されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

## 解決方法

`TrackProperties::has_unknown_mandatory` の判定に GREASE 値の除外を追加し、GREASE 値を Track Property に載せた PUBLISH / SUBSCRIBE_OK / FETCH_OK を unknown mandatory として拒否しないようにした。

- `src/track_properties.rs`: 判定クロージャに `!crate::grease::is_grease(prop_type)` を追加した (mutable リストと IMMUTABLE_PROPERTIES 内側の両方に適用される)。doc には §3.6 の字面 (範囲全体を Mandatory とする) と §16.8 Table 14 の GREASE 行 (Scope Any) が衝突することを明記し、§13 (Grease) の
  "Endpoints MUST NOT close the session solely because they received an unknown value." と §16.8 の "Endpoints MUST ignore unknown Property types, skipping them according to the Key-Value-Pair encoding" を根拠に Table 14 を優先する解釈を書いた。Object scope (`src/object_properties.rs`) も同じ解釈であり、相互参照を doc に残した
- 追加したテスト
  - `tests/test_track_properties.rs`: GREASE の境界 (0x401D / 0x409C / 0x7F9D) を前提 assert 付きで固定、0x4000-0x7FFF 全域のスキャンで「GREASE 値だけを除外する」ことを固定、GREASE 値を encode / decode して往復後も unknown mandatory にならないこと、IMMUTABLE_PROPERTIES 内側の GREASE 値、GREASE 以外の未知 Mandatory の非退行
  - `tests/test_session/parameter_rules.rs`: GREASE 値の Track Property を含む PUBLISH が REQUEST_ERROR (UNSUPPORTED_EXTENSION) にならず Pending(Publisher) として受理されること
  - `tests/test_session/subscription/handshake.rs`: GREASE 値の Track Property を含む SUBSCRIBE_OK が購読キャンセルにならないこと
  - `tests/test_session/fetch/validation.rs`: GREASE 値の track property を含む FETCH_OK が fetch キャンセルにならないこと
- `CHANGES.md` の `## develop` に `[FIX]` エントリを追加した

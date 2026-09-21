# GREASE 値が Mandatory Track Property 範囲に入る場合に unknown mandatory としない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-grease-track-property-mandatory-range
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-transport-21 §16.8 (Properties) の Table 14 は GREASE 用に予約された Property Type を `0x7f * N + 0x9D` として Scope を Any と定め、§13 (Grease) は
「未知値を受けただけでセッションを閉じてはならない MUST NOT」と規定する。一方 §3.6 (Mandatory Track Properties) は 0x4000-0x7FFF を Mandatory Track Property とし、
理解できない Mandatory Track Property を受けた端末に「その Track を処理も転送もしてはならない」を課す。

GREASE 値のうち N = 128〜255 (0x401D〜0x7F1E) はこの範囲に重なるため、現状は GREASE を送る peer の Track を購読できない。Object scope 側は [issues/0121](../issues/0121-bug-grease-property-mandatory-range.md) で扱う。本 issue は Track scope 側を扱う。

## 現状

- `src/track_properties.rs` の `TrackProperties::has_unknown_mandatory` は `MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX` の範囲判定だけを行い、既知の Mandatory Track Property 以外を unknown mandatory とみなす。`src/grease.rs` の `is_grease` による除外が無い
- 呼び出し元は `src/session/subscription/recv.rs` の `Session::handle_peer_publish` と `Session::handle_peer_subscribe_ok`、
  `src/session/fetch.rs` の `Session::handle_peer_fetch_ok` である。検出すると §3.6 に従って該当の購読 / fetch を終了する
  (cancel の手段は [issues/0110](../issues/0110-bug-cancel-request-on-mandatory-track-property.md) で扱う)
- GREASE 値は偶奇が交互になるため、varint 値の型と長さ付きバイト列の型の両方が 0x4000-0x7FFF に入る。`TrackProperties::decode` はどちらも受理する
- 0x4000-0x7FFF の Track Property を与える既存テストは無く、この分岐は未テストである

## 設計方針

- `TrackProperties::has_unknown_mandatory` の判定で `src/grease.rs` の `is_grease` を除外し、GREASE 値は未知 Property として §8.4 どおり保持・転送する
- 同じ衝突は Object scope の `src/object_properties.rs` にもある。判定の実装を共有するか、少なくとも同じ解釈であることを doc コメントで示す ([issues/0121](../issues/0121-bug-grease-property-mandatory-range.md) と同時に対応するのが望ましい)
- draft 内部の矛盾 (§3.6 が範囲全体を Mandatory とするのに対し、Table 14 が GREASE の Scope を Any とする) を issue に記録し、採用した解釈の根拠 (§13 の MUST NOT と Table 14) を明示する

## 完了条件

- GREASE 値 (0x401D〜0x7F1E のいずれか) を Track Property として含む PUBLISH / SUBSCRIBE_OK / FETCH_OK を受信しても、unknown mandatory として扱わないことを固定するテストが追加されていること
- GREASE 以外の未知 Mandatory Track Property は従来どおり §3.6 の対象であり続けることを固定するテストが追加されていること
- 偶奇両方の GREASE 値 (varint 型と長さ付きバイト列型) を覆っていること
- Object scope 側 ([issues/0121](../issues/0121-bug-grease-property-mandatory-range.md)) と同じ解釈で実装されていること
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること

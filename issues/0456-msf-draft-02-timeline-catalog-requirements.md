# Timeline 用カタログ要件を v02 に合わせる

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §7.2 (Media Timeline Catalog requirements) と §8.3 (Event Timeline Catalog requirements) は、v01 のカタログ要件を変更した。

- media timeline は `depends` に、適用対象の track name と renderGroup を MUST で持つ。renderGroup 内の全 track が一致する場合は renderGroup を MUST 参照し個別 track 名を MUST NOT 参照する。一部だけ一致する場合はその逆とする。
- event timeline は、index が参照する media timeline track がある場合に限り、その track を `depends` に MUST で列挙する。
- `mimeType` が `"application/json"` である MUST は両方から削除された。

現行実装は media timeline / event timeline の両方に `depends` 非空と `mimeType == "application/json"` を要求しており、v02 のカタログを誤って拒否する。

## 現状

- `src/msf.rs` の `validate_full_track` と `decode_track`、`MsfCloneTrack::into_track` は、media timeline / event timeline に `depends` 非空と `mimeType == "application/json"` を要求する。
- event timeline の `eventType` 必須検証は実装済みである。
- `MsfTrack.depends` は `Vec<String>` で、renderGroup 参照を表現できない。renderGroup 対応は別 issue で扱う。
- index が media timeline を参照するかどうかを判定する処理はない。`MsfEventTimeline` は entry 列だけを扱う。
- 参照 track の publication 停止時に `depends` を更新する処理はない。timeline の配信経路自体が未実装である。

## 設計方針

- `mimeType == "application/json"` の必須検証を削除する。`mimeType` が付いていても値の検証はしない。
- media timeline の「`depends` 非空」は維持し、参照先が track name か renderGroup かで §7.2 の使い分け規則を検証する。renderGroup 参照の型は depends の renderGroup 対応 issue に依存するため、本 issue はその issue の後に実施する。
- event timeline の `depends` は必須を外し、index が media timeline を参照する場合だけ必須にする。index の参照先を判定する API が `MsfEventTimeline` にない場合は、判定に必要な情報を追加する。
- 参照 track 停止時の `depends` 更新は publisher の運用責務であり、timeline の配信経路が未実装のため本 issue では実装しない。完了条件に含めない理由を issue に残す。

## 完了条件

- `mimeType` を持たない media timeline / event timeline の track が受理されること。
- media timeline で `depends` が空の場合が拒否されること。
- renderGroup 内の全 track が一致する場合に個別 track 名だけを参照した media timeline が拒否され、renderGroup 参照が受理されること。一部だけ一致する場合はその逆になること。
- event timeline で index が media timeline を参照しない場合に `depends` なしで受理され、参照する場合に `depends` 欠如が拒否されること。
- 上記を `tests/test_msf/catalog_decode.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

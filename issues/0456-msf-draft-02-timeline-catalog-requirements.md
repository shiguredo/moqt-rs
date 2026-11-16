# Timeline 用カタログ要件を v02 に合わせる

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §7.2 (Media Timeline Catalog requirements) と §8.3 (Event Timeline Catalog requirements) は、v01 のカタログ要件を変更した。

- media timeline は `depends` に、適用対象の track name と renderGroup を MUST で持つ。
- renderGroup 内の全 track が一致する場合は renderGroup を MUST 参照し、個別 track 名を MUST NOT 参照する。一部だけ一致する場合は renderGroup を MUST NOT 参照し、個別 track を MUST 参照する。
- event timeline は、index が参照する media timeline track がある場合に限り、その track を `depends` に MUST で列挙する。
- `mimeType` が `"application/json"` である MUST は両方から削除された。

現行実装は media timeline / event timeline の両方に `depends` 非空と `mimeType == "application/json"` を要求しており、v02 のカタログを誤って拒否する。

## 現状

- `src/msf.rs` の `validate_full_track` と `decode_track`、`MsfCloneTrack::into_track` は、media timeline / event timeline に `depends` 非空と `mimeType == "application/json"` を要求する。
- event timeline の `eventType` 必須検証は実装済みである。
- `MsfTrack.depends` は `Vec<String>` で、renderGroup 参照を表現できない。renderGroup 対応は 0445 が担当する。
- event timeline の index 種別は payload の `MsfEventIndex` にあり、track object にはない。catalog の decode だけでは index が media timeline を参照するかを判定できない。
- catalog の `depends` だけからは「renderGroup 内の一部だけが一致する」ことを表現できない。renderGroup を参照した時点で全 track が対象になる。
- 参照 track の publication 停止時に `depends` を更新する処理はない。timeline の配信経路自体が未実装である。

## 設計方針

- `mimeType == "application/json"` の必須検証を削除する。`mimeType` が付いていても値の検証はしない。
- media timeline の「`depends` 非空」は維持する。0445 の完了後に、catalog 全体を見る検証を `decode_full_catalog`、`validate_full_catalog_for_encode`、`validate_after_delta` に追加する。
- catalog 全体の検証で次を拒否する: renderGroup の全 track を個別名で列挙し renderGroup を参照しない `depends`、renderGroup を参照しながらその所属 track を個別名でも列挙する `depends`。renderGroup の一部だけが一致する場合の renderGroup 参照は catalog から判定できないため対象外とし、publisher の責務として doc に明記する。
- event timeline の `depends` は必須を外す。index 参照の判定は catalog だけではできないため、`MsfCatalog` と `MsfEventTimeline` を受け取る公開検証 API を追加する。`MsfEventIndex::MediaPtsMs` の entry がある場合に、`depends` が少なくとも 1 つの media timeline track を含むことを検証する。
- 上記の API は、catalog と event timeline の両方が手元にある呼び出し側 (crates/moq-sub など) から使う。catalog の decode からは呼ばない。
- 参照 track 停止時の `depends` 更新は publisher の運用責務であり、timeline の配信経路が未実装のため本 issue では実装しない。完了条件に含めない理由を issue に残す。
- 実施順序: 0445 (depends の型と renderGroup 参照) の後に実施する。

## 完了条件

- `mimeType` を持たない media timeline / event timeline の track が受理されること。
- media timeline で `depends` が空の場合が拒否されること。
- renderGroup の全 track を個別名で列挙し renderGroup を参照しない media timeline が、decode と encode の両方で拒否されること。
- renderGroup を参照しながら所属 track を個別名でも列挙する media timeline が拒否されること。
- event timeline で index が `MediaPtsMs` を持たない場合に `depends` なしで受理され、`MediaPtsMs` を持つ場合に media timeline track を含まない `depends` が検証 API で拒否されること。
- `MsfCatalog` と `MsfEventTimeline` を受け取る検証 API が公開されていること。
- renderGroup の一部だけが一致する場合を catalog から判定しないことを doc に明記すること。
- 上記を `tests/test_msf/catalog_decode.rs`、`tests/test_msf/delta_apply.rs`、`tests/test_msf/event_timeline.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

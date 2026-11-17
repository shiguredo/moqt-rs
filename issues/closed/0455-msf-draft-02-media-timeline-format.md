# MSF Media Timeline の format 排他と media time 順序を強制する

- Created: 2026-11-16
- Completed: 2026-11-17
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §7.1 (Media Timeline track payload) は、v01 の「explicit 形式と template 形式を 1 つの document で併用してよい (MAY)」を削除し、「media timeline track は両方の形式を使ってはならない (MUST NOT)」に変更した。また、media timeline track は media time の順に並ぶことを定める (§7.1.1)。

ここでいう 2 つの形式は、catalog の `template` 属性 (§5.2.16) を使う形式と、explicit な media timeline payload を使う形式である。§5.2.16 は template 属性を持つ track が別の media timeline track を持つことを SHOULD NOT とするが、これは 1 つの track と payload の組からは判定できないため本 issue の範囲外とする。

現行実装は payload の並び順を検証せず、template 属性と explicit payload の併用も検出できない。

## 現状

- `MsfMediaTimeline::decode` は JSON 配列をそのまま `Vec<MsfMediaTimelineEntry>` に取り込み、`pts_ms` が非降順かどうかを検証しない。
- `MsfMediaTimeline::encode` は `Vec<u8>` を返す infallible な関数で、順序違反を拒否できない。`encode_media_timeline` は `Result` を返す。
- template 形式は `MsfTrack.template: Option<MsfTemplate>` (§5.2.16) として表現され、explicit payload は `MsfMediaTimeline` として別の型である。両者が同時に存在することを検出する API はない。
- `pbt/tests/prop_msf.rs` の media timeline の roundtrip は、順序を保証しない entry を生成している。decode で順序を検証すると、この生成器が失敗する。
- `docs/missing-features.md` は v01 の「併用してよい (MAY)」を前提にした記述のままである。
- `issues/0353-change-msf-timeline-number-type.md` が時刻フィールドの型を扱う。`pts_ms` の型は本 issue では変えない。

## 設計方針

- `MsfMediaTimeline::decode` で `pts_ms` が非降順 (同値を許容) であることを検証し、違反を `MessageError::InvalidCatalog` にする。
- `MsfMediaTimeline::encode` は infallible のままにし、順序検証は `encode_media_timeline` で行う。`encode` の doc に「順序検証は呼び出し側の `encode_media_timeline` が行う」ことを明記する。
- template 属性と explicit payload の併用を検出する公開検証 API を追加する。`MsfTrack` と `MsfMediaTimeline` を受け取り、`template` が設定されている track に explicit payload を紐付けた場合に `MessageError::InvalidCatalog` を返す。
- §5.2.16 の cross-track の SHOULD NOT は、1 つの track と payload の組では判定できないため対象外とし、その旨を doc に明記する。
- `pbt/tests/prop_msf.rs` の media timeline の生成器を、`pts_ms` が非降順になるように変更する。
- `docs/missing-features.md` と `docs/msf.md` の該当記述を v02 に合わせて更新する。

## 完了条件

- `pts_ms` が降順になる `MsfMediaTimeline` の decode が `MessageError::InvalidCatalog` で拒否されること。
- `pts_ms` が同値の要素を持つ `MsfMediaTimeline` が受理されること (非降順を許容)。
- `encode_media_timeline` が降順の `MsfMediaTimeline` を `MessageError::InvalidCatalog` で拒否すること。
- template 属性を持つ track に explicit payload を紐付けた検証 API が `MessageError::InvalidCatalog` を返すこと。
- template 属性を持たない track の explicit payload が受理されること。
- `pbt/tests/prop_msf.rs` の生成器が非降順の entry を生成し、roundtrip が通ること。
- `docs/missing-features.md` と `docs/msf.md` の記述が更新されていること。
- 上記を `tests/test_msf/media_timeline.rs`、`tests/test_msf/encode_decode_roundtrip.rs`、`pbt/tests/prop_msf.rs` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

## 解決方法

- `MsfMediaTimeline::decode` と `encode_media_timeline` で `pts_ms` が非降順であることを検証し、違反を `InvalidCatalog` にするようにした。同値は許容する。`MsfMediaTimeline::encode` は infallible のままにし、順序検証は `encode_media_timeline` が行うことを doc に明記した。
- `validate_media_timeline_format(track, timeline)` を追加し、`template` 属性を持つ track に explicit payload を紐付けた場合と、media time が降順の場合を拒否するようにした。§5.2.16 の cross-track の SHOULD NOT は 1 つの track と payload の組から判定できないため対象外であることを doc に明記した。
- `pbt/tests/prop_msf.rs` の media timeline 生成器を `pts_ms` の非降順に変更した。
- `tests/test_msf/media_timeline.rs` に降順拒否、同値受理、template 併用拒否のテストを追加した。`docs/msf.md` と `docs/missing-features.md` の v01 前提の記述を v02 に更新した。

確認コマンド: `cargo test --workspace`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo fmt --all -- --check` が通ることを確認した。

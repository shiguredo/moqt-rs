# MSF Media Timeline の format 排他と media time 順序を強制する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-02 §7.1 (Media Timeline track payload) は、v01 の「explicit 形式と template 形式を 1 つの document で併用してよい (MAY)」を削除し、「media timeline track は両方の形式を使ってはならない (MUST NOT)」に変更した。また、media timeline track は media time の順に並ぶことを定める。
現行実装は payload の並び順を検証せず、template 属性と explicit payload の併用も検出できない。

## 現状

- `MsfMediaTimeline::decode` は JSON 配列をそのまま `Vec<MsfMediaTimelineEntry>` に取り込み、`pts_ms` が昇順かどうかを検証しない。`MsfMediaTimeline::encode` も順序違反を拒否できない。
- template 形式は `MsfTrack.template: Option<MsfTemplate>` (§5.2.16) として表現され、explicit payload は `MsfMediaTimeline` として別の型である。両者が同時に存在することを検出する API はない。
- `MsfTemplate::resolve_entry` は §7.4.1 の計算式を実装している。Group ID の非連続・非単調を制約していない点は v02 と一致する。
- timeline の配信・購読経路は未実装で、`MsfMediaTimeline` は codec と tests / pbt からのみ参照されている。

## 設計方針

- `MsfMediaTimeline::decode` で `pts_ms` が非降順であることを検証し、違反を `MessageError::InvalidCatalog` にする。`encode` でも同じ検証を行う。
- template 属性と explicit payload の併用を検出する検証 API を追加する。catalog の `MsfTrack` と payload の `MsfMediaTimeline` の両方を受け取り、`template` が設定されている track に explicit payload を紐付けた場合に `MessageError::InvalidCatalog` を返す形にする。
- 検証 API は将来の配信・購読経路から呼べるよう公開する。現時点ではテストから呼ぶ。
- `issues/0353-change-msf-timeline-number-type.md` が時刻フィールドの型を扱っているため、`pts_ms` の型は本 issue では変えない。

## 完了条件

- `pts_ms` が降順になる `MsfMediaTimeline` の decode と encode が `MessageError::InvalidCatalog` で拒否されること。
- `pts_ms` が同値の要素を許容するか拒否するかを決め、テストで固定すること。
- template 属性を持つ track に explicit payload を紐付けた検証が `MessageError::InvalidCatalog` を返すこと。
- template 属性を持たない track の explicit payload が受理されること。
- 上記を `tests/test_msf` で固定し、`cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

# renderGroup の targetLatency / buffers の「欠如」を不一致として拒否しない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-render-group-missing-value
- Polished: {YYYY-MM-DD}

## 目的

draft-ietf-moq-msf-01 §5.2.8 (Target latency) は同一 render group / alternate group 内の targetLatency 一致を MUST とする一方、フィールドが無い場合を player の裁量に委ねる。

> All tracks belonging to the same render group MUST have identical target latencies.
> All tracks belonging to the same alternate group MUST have identical target latencies.
> If this field is absent from the track definition, and isLive is TRUE, then the player MAY choose the latency with which it renders the content.

§5.2.9 (Buffers) も同じ構造をとる。

> All tracks belonging to the same render group MUST have identical target buffers.
> All tracks belonging to the same alternate group MUST have identical target buffers.
> If this field is absent from the track definition, and isLive is TRUE, then the player MAY choose the buffers with which it conducts playback.

MUST が要求するのは宣言された値の同一性であり、省略は player が選んでよい (MAY) と明示されている。現状は省略 (`None`) を 1 つの値として比較するため、片方が省略されただけのカタログを `InvalidCatalog` として拒否し、仕様が player に残した余地を潰している。

## 現状

`src/msf.rs` の `validate_group_target_latency` は `hashbrown::HashMap<u64, Option<u64>>` に group ごとの `MsfTrack::target_latency` を格納し、`Entry::Occupied` で `*e.get() != t.target_latency` を比較する。`validate_group_buffers` も同じ構造で `Option<MsfBuffers>` を比較する。`None` は他のどの値とも等しくならないため、宣言と省略の混在が不一致として扱われる。

確認した挙動は次のとおり。

- 同一 renderGroup の 2 トラックで片方だけ `targetLatency` を宣言したカタログは `InvalidCatalog` になる
- 同一 renderGroup の 2 トラックで片方だけ `buffers` を宣言したカタログは `InvalidCatalog` になる
- 両方が異なる値を宣言したカタログも `InvalidCatalog` になる (こちらは MUST 違反で正しい)

これらの検証は `decode_full_catalog`、`MsfCatalog::validate_after_delta`、`validate_full_catalog_for_encode`、`validate_delta_for_encode` から呼ばれるため、decode と encode の両経路で同じ挙動になる。

## 設計方針

group ごとに宣言された値の集合だけを比較対象にし、省略を不一致として扱わない。

- 値が `Some` のトラックだけを比較に加える。group 内で 2 つ以上の異なる値が宣言された場合は MUST 違反として従来どおり `InvalidCatalog` を返す
- 値が `None` のトラックは比較に加えない。省略は §5.2.8 / §5.2.9 が player の裁量 (MAY) に委ねているため、他のトラックの宣言値との同一性を要求できない
- MUST の解釈: "All tracks belonging to the same render group MUST have identical target latencies." は、targetLatency を宣言したトラック同士に適用される。省略したトラックは player が選んだ値を使うため、宣言値と一致する義務を負わない
- `isLive=false` のトラックを比較対象外とする既存の扱い (§5.2.8 の "If isLive is FALSE, this field MUST be ignored.") は維持する
- `buffers` は `Option<MsfBuffers>` の外側だけを対象とする。`MsfBuffers` 内部の `target` / `min` / `max` が省略されている場合 (§5.2.9 の "Keys are optional.") の比較規則は変更しない
- 同じ扱いを renderGroup と altGroup、`tracks` と `publishTracks` の全呼び出しに適用する

targetLatency を消費する subscriber 側の扱い (省略時に何を再生遅延に選ぶか) は [issues/0104](../issues/0104-add-subscriber-av-sync.md) が扱う。本 issue は検証の修正のみを対象とする。

## 完了条件

- 同一 renderGroup の 2 トラックで片方だけ `targetLatency` を宣言したカタログを `MsfCatalogDocument::decode` が受理するテストが `tests/test_msf/error_cases.rs` に追加されていること
- 同一 renderGroup の 2 トラックで片方だけ `buffers` を宣言したカタログを `MsfCatalogDocument::decode` が受理するテストが追加されていること
- 同一 altGroup でも同様に受理するテストが追加されていること
- encode 経路 (`MsfCatalog` を手組みして `MsfCatalogDocument::encode`) でも受理するテストが追加されていること
- delta 適用後 (`MsfCatalog::apply_delta`) の検証でも受理するテストが `tests/test_msf/delta_apply.rs` に追加されていること
- 2 つ以上の異なる値が宣言された場合を拒否する既存テスト (`render_group_different_target_latency_rejected` / `alt_group_different_target_latency_rejected` /
  `render_group_different_buffers_rejected` / `alt_group_different_buffers_rejected` / `encode_full_group_target_latency_mismatch_rejected` / `encode_full_publish_tracks_group_buffers_mismatch_rejected`) が維持されていること
- `isLive=false` のトラックが比較対象外である既存テスト (`render_group_mixed_live_ignored_for_buffers` / `render_group_mixed_live_ignored_for_target_latency`) が維持されていること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

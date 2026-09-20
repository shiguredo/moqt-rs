# renderGroup の targetLatency / buffers の「欠如」を不一致として拒否しない

- Created: 2026-09-21
- Completed: 2026-09-23
- Branch: feature/fix-msf-render-group-missing-value
- Polished: 2026-09-22

## 目的

draft-ietf-moq-msf-01 §5.2.8 (Target latency) は同一 render group / alternate group 内の targetLatency 一致を MUST とする一方、フィールドが無い場合を player の裁量に委ねる。

> All tracks belonging to the same render group MUST have identical target latencies.
> All tracks belonging to the same alternate group MUST have identical target latencies.
> If this field is absent from the track definition, and isLive is TRUE, then the player MAY choose the latency with which it renders the content.

§5.2.9 (Buffers) も同じ構造をとる。

> All tracks belonging to the same render group MUST have identical target buffers.
> All tracks belonging to the same alternate group MUST have identical target buffers.
> If this field is absent from the track definition, and isLive is TRUE, then the player MAY choose the buffers with which it conducts playback.

MUST が要求するのは宣言された値の同一性であり、省略は player が選んでよい (MAY) と明示されている。現状は省略 (`None`) を 1 つの値として比較するため、どちらも `isLive=true` のトラックで片方が省略されただけのカタログを `InvalidCatalog` として拒否し、仕様が player に残した余地を潰している。

## 現状

`src/msf.rs` の `validate_group_target_latency` は `hashbrown::HashMap<u64, Option<u64>>` に group ごとの `MsfTrack::target_latency` を格納し、`Entry::Occupied` で `*e.get() != t.target_latency` を比較する。`validate_group_buffers` も同じ構造で `Option<MsfBuffers>` を比較する。`None` は他のどの値とも等しくならないため、宣言と省略の混在が不一致として扱われる。

確認した挙動は次のとおり。

- 同一 renderGroup の 2 トラックがどちらも `isLive=true` で、片方だけ `targetLatency` を宣言したカタログは `InvalidCatalog` になる
- 同一 renderGroup の 2 トラックがどちらも `isLive=true` で、片方だけ `buffers` を宣言したカタログは `InvalidCatalog` になる
- 宣言しない側が `isLive=false` の場合は元から受理される (`validate_group_target_latency` が先頭の `if !t.is_live { continue; }` で除外するため)。この既存の扱いは設計方針で維持する
- 両方が異なる値を宣言したカタログも `InvalidCatalog` になる (こちらは MUST 違反で正しい)

`validate_group_target_latency` / `validate_group_buffers` の呼び出しは `decode_full_catalog` / `MsfCatalog::validate_after_delta` / `validate_full_catalog_for_encode` の 3 箇所である。
`validate_delta_for_encode` はトラック単位の検証だけで group 検証を呼ばず、delta の group 検証は `apply_delta` 経由の `validate_after_delta` のみで行われる。
decode 経路 (`decode_full_catalog`)、encode 経路 (`validate_full_catalog_for_encode`)、delta 適用後 (`validate_after_delta`) が同じ挙動になる。

## 設計方針

group ごとに宣言された値の集合だけを比較対象にし、省略を不一致として扱わない。

- 値が `Some` のトラックだけを比較に加える。group 内で 2 つ以上の異なる値が宣言された場合は MUST 違反として従来どおり `InvalidCatalog` を返す
- 値が `None` のトラックは比較に加えない。省略は §5.2.8 / §5.2.9 が player の裁量 (MAY) に委ねているため、他のトラックの宣言値との同一性を要求できない
- MUST の解釈 (本実装の解釈): "All tracks belonging to the same render group MUST have identical target latencies." は、targetLatency を宣言したトラック同士に適用されると解釈する。
  原文の主語は宣言の有無を限定しないため逐語からは一意に決まらないが、(1) 省略時は player が値を選ぶ (MAY) ため player が宣言値と同じ値を選べば MUST は満たせる、(2) §5.2.8 / §5.2.9 は同一トラックでの targetLatency と buffers の同居を禁じるため、省略を不一致として扱うと group 内でフィールドを使い分ける構成まで拒否してしまう、の 2 点からこの解釈を採る
- `isLive=false` のトラックを比較対象外とする既存の扱い (§5.2.8 の "If isLive is FALSE, this field MUST be ignored.") は維持する
- `buffers` は `Option<MsfBuffers>` の外側だけを対象とする。`MsfBuffers` 内部の `target` / `min` / `max` が省略されている場合 (§5.2.9 の "Keys are optional.") の比較規則は変更しない
- 変更は `validate_group_target_latency` / `validate_group_buffers` の内部で行うため、renderGroup / altGroup と `tracks` / `publishTracks` の全呼び出しに自動的に反映される。
  `validate_after_delta` が `self.tracks` だけを検証する現状 (delta の操作は `tracks` のみを変更する) と、`validate_delta_for_encode` が group 検証を行わない現状は変更しない

targetLatency を消費する subscriber 側の扱い (省略時に何を再生遅延に選ぶか) は [issues/0104](../issues/0104-add-subscriber-av-sync.md) が扱う。本 issue は検証の修正のみを対象とする。

## 完了条件

- 同一 renderGroup の 2 トラック (どちらも `isLive=true`) で片方だけ `targetLatency` を宣言したカタログを `MsfCatalogDocument::decode` が受理するテストが `tests/test_msf/error_cases.rs` に追加されていること (宣言しない側を `isLive=false` にすると修正前から通ってしまうため `isLive=true` を明示する)
- 同一 renderGroup の 2 トラック (どちらも `isLive=true`) で片方だけ `buffers` を宣言したカタログを `MsfCatalogDocument::decode` が受理するテストが追加されていること (同じく `isLive=true` を明示する)
- 同一 altGroup でも同様 (どちらも `isLive=true`) に受理するテストが追加されていること
- encode 経路 (`MsfCatalog` を手組みして `MsfCatalogDocument::encode`) でも受理するテストが追加されていること
- delta 適用後 (`MsfCatalog::apply_delta`) の検証でも受理するテストが `tests/test_msf/delta_apply.rs` に追加されていること
- 2 つ以上の異なる値が宣言された場合を拒否する既存テスト (`render_group_different_target_latency_rejected` / `alt_group_different_target_latency_rejected` /
  `render_group_different_buffers_rejected` / `alt_group_different_buffers_rejected` / `encode_full_group_target_latency_mismatch_rejected` / `encode_full_publish_tracks_group_buffers_mismatch_rejected`) が維持されていること
- `isLive=false` のトラックが比較対象外である既存テスト (`render_group_mixed_live_ignored_for_buffers` / `render_group_mixed_live_ignored_for_target_latency`) が維持されていること。
  ただし decode 経路は `decode_track` が `isLive=false` の `targetLatency` / `buffers` を `None` に正規化するため、この 2 テストは変更後の「省略を比較しない」規則だけでも通る。
  そのため isLive による除外そのものは正規化を通らない経路で固定する。手組みの `MsfCatalog` (`is_live=false` かつ別の値を持つ `MsfTrack` を同一 group に置く) を `MsfCatalogDocument::encode` するテスト、または `apply_delta` に同じ `MsfTrack` を渡すテストを追加すること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

## 解決方法

`src/msf.rs` の `validate_group_target_latency` / `validate_group_buffers` を、グループ内で宣言された値だけを比較するよう修正した。

- 比較用の map を `HashMap<u64, Option<u64>>` / `HashMap<u64, Option<MsfBuffers>>` から `HashMap<u64, u64>` / `HashMap<u64, MsfBuffers>` に変更し、省略 (`None`) のトラックは比較に加えない
- `isLive=false` のトラックを比較対象外とする既存の扱いは維持する
- 変更は共有ヘルパーの内部で行うため、decode (`decode_full_catalog`)、encode (`validate_full_catalog_for_encode`)、delta 適用後 (`MsfCatalog::validate_after_delta`) の 3 経路に自動的に反映される
- `MsfBuffers` 内部の `target` / `min` / `max` の省略規則と、`validate_delta_for_encode` が group 検証を行わない現状は変更しない
- `apply_delta` の `# Errors`、`validate_after_delta` / `comparable_attributes` のコメント、`docs/IMPLEMENTATION.md` の該当記述を新しい規則に追随させた

追加・更新したテスト:

- `tests/test_msf/error_cases.rs`
  - decode 受理:
    - `render_group_target_latency_omitted_accepted`
    - `alt_group_target_latency_omitted_accepted`
    - `render_group_buffers_omitted_accepted`
    - `alt_group_buffers_omitted_accepted`
    - `render_group_target_latency_and_buffers_split_accepted`
  - decode 拒否:
    - `render_group_omitted_between_different_target_latency_rejected`
    - `render_group_omitted_between_different_buffers_rejected`
  - encode 受理:
    - `encode_full_group_target_latency_omitted_accepted`
    - `encode_full_group_buffers_omitted_accepted`
    - `encode_full_alt_group_target_latency_omitted_accepted`
    - `encode_full_alt_group_buffers_omitted_accepted`
    - `encode_full_publish_tracks_group_target_latency_omitted_accepted`
    - `encode_full_without_group_different_target_latency_accepted`
  - encode 拒否:
    - `encode_full_group_omitted_between_different_target_latency_rejected`
    - `encode_full_group_omitted_between_different_buffers_rejected`
    - `encode_full_group_render_conflict_rejected`
    - `encode_full_group_alt_conflict_rejected`
    - `encode_full_group_zero_target_latency_mismatch_rejected`
    - `encode_full_group_zero_buffers_mismatch_rejected`
  - isLive 除外 (decode の正規化を通らない encode 経路で固定):
    - `encode_full_group_target_latency_is_live_false_ignored`
    - `encode_full_group_buffers_is_live_false_ignored`
  - `render_group_mixed_live_ignored_for_buffers` / `render_group_mixed_live_ignored_for_target_latency` のコメントを、固定対象が実態と合うよう修正
- `tests/test_msf/delta_apply.rs`
  - 受理:
    - `apply_delta_group_target_latency_omitted_accepted`
    - `apply_delta_group_buffers_omitted_accepted`
    - `apply_delta_alt_group_target_latency_omitted_accepted`
    - `apply_delta_alt_group_buffers_omitted_accepted`
  - 拒否:
    - `apply_delta_group_omitted_between_different_target_latency_rejected`
    - `apply_delta_group_omitted_between_different_buffers_rejected`
    - `apply_delta_alt_group_omitted_between_different_target_latency_rejected`
    - `apply_delta_alt_group_omitted_between_different_buffers_rejected`
  - `apply_delta_clone_inherited_group_conflict_rejected` / `apply_delta_readd_omitted_target_latency_rejected`

`make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ることを確認した。

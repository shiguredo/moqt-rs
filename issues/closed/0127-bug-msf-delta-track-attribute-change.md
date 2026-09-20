# MSF の delta update で同一 Track の属性変更を検出しない

- Created: 2026-09-21
- Completed: 2026-09-23
- Branch: feature/fix-msf-delta-track-attribute-change
- Polished: 2026-09-22

## 目的

draft-ietf-moq-msf-01 §5.3 (Delta updates) は、Track Namespace と Track Name の組が固定の Track 属性を定義し、宣言後に変更してはならないと規定する。

> The tuple of Track Namespace and Track Name defines a fixed set of Track attributes which MUST NOT be modified after being declared.  To modify any attribute, a new track with a different Namespace|Name tuple is created by Adding or Cloning and then the old track is removed.

同節が delta update で許す操作は「未宣言の Track の追加」「宣言済み Track の clone による追加」「宣言済み Track の削除」に限られる。

```text
A restricted set of operations are allowed with each delta update: * Add a new track that has not previously been declared. * Add a new track by cloning a previously declared track. * Remove a track that has been previously declared.
```

また §5.2.7 (Is Live) は isLive の逆行を禁じる。

> A True value MUST never follow a False value.

現状の `MsfCatalog::apply_delta` は、同一 (namespace, name) を remove してから add / clone すると属性を自由に変更でき、isLive も false から true へ戻せる。MUST に違反する delta update を library が受理するため、属性不変を前提にカタログを解釈する subscriber が壊れる。

## 現状

`src/msf.rs` の `MsfCatalog::apply_delta` は `self` の複製に `MsfCatalog::apply_delta_in_place` を適用し、全操作と `MsfCatalog::validate_after_delta` が成功したときだけ差し替える。各操作の実装は次のとおり。

- `MsfDeltaOperation::Remove` は `MsfCatalog::find_track_index` で見つけた `MsfCatalog::tracks` の要素を削除するだけで、削除した Track の属性をどこにも残さない
- `MsfDeltaOperation::Add` は `find_track_index` で現在の `tracks` との名前重複を検査して `tracks` へ push する。`publish_tracks` は `find_track_index` の走査対象ではなく、そちらとの重複は `validate_after_delta` が検出する
- `MsfDeltaOperation::Clone` は親を `MsfCloneTrack::into_track` で解決したうえで、同じく `find_track_index` による `tracks` 内の名前重複だけを検査して push する
- `validate_after_delta` は名前の一意性、`initRef`、同一 group の `targetLatency` / `buffers` 一致を検証するだけで、削除済み Track を参照しない

`MsfCatalog` には削除済み Track の履歴を持つフィールドが無い。このため次の delta update がすべて成功する。

- remove → add で `label` / `codec` / `width` などの属性を変更する (`targetLatency` / `buffers` は同一 renderGroup / altGroup に isLive=true のトラックが残る場合 `validate_after_delta` が一致を検査するため例から外す)
- remove → clone で解放された名前を再利用し、親から継承した属性で元と異なる属性にする
- remove → add で `isLive` を false から true にする (§5.2.7 違反)

remove → add の属性変更を検証する既存テストは無い。`pbt/tests/prop_msf_delta.rs` の `add_then_remove_restores_tracks` は add → remove の可逆性だけを検証している。

## 設計方針

削除した Track の属性を履歴として `MsfCatalog` に保持し、同じ (namespace, name) が再追加されたときに属性変更と isLive の逆行を検出する。

- 履歴は `MsfCatalog` のフィールドとして持つ。1 回の `apply_delta` の呼び出し内だけで判断すると、delta update をまたいだ remove → add を検出できない
- キーは `MsfCatalog::find_track_index` と同じ規則で解決した (namespace, name) とする。namespace 省略時は catalog の namespace を継承したものとして比較する (draft-ietf-moq-msf-01 §5.2.2 (Track namespace))
- 値は削除された `MsfTrack` を 1 件保持する。属性変更の検出には属性集合そのものが必要であり、`MsfTrack` は `Clone` と `PartialEq` を実装済みのため、エンコード文字列やハッシュへ落とすより取りこぼしが無い
- 履歴は (namespace, name) ごとに 1 エントリとし、同じ tuple が再追加されても削除前の属性を保持し続ける。削除しても消えないため、add して remove した tuple の数だけ単調増加する (上限・破棄規則は設けない)。`examples/moqt-subscriber` は Full catalog の受信で catalog ごと差し替えるため、増加は 1 つの catalog インスタンスの生存期間に限られる
- 再追加時は name と namespace を除く属性を比較し、差分があれば `InvalidCatalog` を返す。Add と Clone の両経路を対象にする
- 比較は実効的な属性で行う。`isLive=false` のとき `targetLatency` / `buffers` は §5.2.8 (Target latency) / §5.2.9 (Buffers) により無視されるため、比較の両辺で `isLive=false` なら `targetLatency` / `buffers` を `None` とみなす。
  `MsfCloneTrack::into_track` と decoder は `None` へ正規化するが、Add 経路は手組みの `MsfTrack` を正規化せず `tracks` へ push する (`validate_full_track` は `targetLatency` / `buffers` と `isLive` について共存禁止と `trackDuration` しか検査しない)。
  そのため比較側で正規化しないと結果が `MsfTrack` の構築経路に依存する
- `isLive` の false → true は §5.2.7 の独立した MUST のため、違反した MUST を特定できる専用メッセージで `InvalidCatalog` を返す。属性比較でも検出できるが、エラー理由を区別できるようにする
- 同一属性での remove → add は本 issue では引き続き受理する。§5.3 の「未宣言の Track の追加」という文言だけを見れば拒否する解釈もあるが、本 issue は属性変更と isLive の逆行の検出に絞る

`MsfCatalog` は全フィールドが public で、構造体リテラルからも構築されている。フィールドを追加するときは `MsfCatalog::new` / `impl Default for MsfCatalog` / `decode_full_catalog` (`src/msf.rs`) に加えて、次のリテラルを更新する。
`tests/test_msf/encode_decode_roundtrip.rs` (13 箇所) / `tests/test_msf/error_cases.rs` (5 箇所) / `pbt/tests/prop_msf.rs` (2 箇所) / `examples/moqt-publisher/src/catalog.rs` (1 箇所)。
`pbt` と `examples/moqt-publisher` は workspace member のため、漏らすと `make test` (`cargo test --workspace`) と `make clippy` (`cargo clippy --workspace --all-targets`) が通らない。
履歴が `PartialEq` の比較対象に含まれるため、カタログ全体を `assert_eq!` で比較する既存テストへの影響も確認する。

subscriber が delta update を継続受信して適用する仕組みは [issues/pending/0006](../issues/pending/0006-add-msf-catalog-subscribe.md) が扱う。本 issue は `MsfCatalog::apply_delta` の検証のみを対象とする。

## 完了条件

- remove → add で `label` / `codec` などの属性を変更した delta update が `InvalidCatalog` になるテストが `tests/test_msf/delta_apply.rs` に追加されていること
- remove → clone で同じ (namespace, name) を再追加し属性が変わる delta update が `InvalidCatalog` になるテストが追加されていること
- remove → add で `isLive` を false から true にする delta update が `InvalidCatalog` になり、そのメッセージが §5.2.7 違反を述べる専用の文言 (`isLive` を含み、属性差分メッセージとは区別できるもの) であることを確認するテストが追加されていること (`tests/test_msf/delta_apply.rs` の既存テストと同じく `reason.contains(...)` で検査する)
- delta update をまたぐ remove → add でも属性変更が検出されるテストが追加されていること (1 回の delta に閉じない)
- 同一属性での remove → add が引き続き成功するテストが追加されていること
- `isLive=false` の Track で `targetLatency` の有無だけが異なる再追加が成功するテストが追加されていること
- `pbt/tests/prop_msf_delta.rs` の `add_then_remove_restores_tracks` と `tests/test_msf/delta_apply.rs` の既存テストがすべて成功すること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

## 解決方法

削除した Track の属性を履歴として `MsfCatalog` に保持し、同じ (namespace, name) が再追加されたときに属性変更と isLive の逆行を検出するようにした。

1. `MsfCatalog` に `removed_tracks: hashbrown::HashMap<(Option<String>, String), MsfTrack>` を追加した。キーは `find_track_index` と同じ規則で解決した (namespace, name) であり、namespace 省略時は catalog の namespace を継承したものとして比較する。値は削除された `MsfTrack` そのもので、属性集合の比較に必要な情報を落とさない。JSON には出力せず、デコード直後は空になる
2. `MsfDeltaOperation::Remove` は削除した Track を履歴へ記録する。`entry().or_insert()` により、同じ tuple が再び削除されても最初に削除されたときの属性を保持し続ける (削除しても消えないため、remove した tuple の数だけ単調増加する。上限・破棄規則は設けない)
3. `Add` / `Clone` の再追加時は `validate_readded_track` で履歴と比較する。`Clone` は `MsfCloneTrack::into_track` が返した解決済みトラック (clone 自身の parentName は残らない) を検査する
4. `isLive` の false → true は §5.2.7 (Is Live) の独立した MUST のため属性差分より先に検査し、`isLive` を含む専用の文言
   (`delta add: track 'v' isLive MUST NOT change from false to true (draft-ietf-moq-msf-01 §5.2.7)`) で `InvalidCatalog` を返す。
   属性変更は `delta {add|clone}: track 'v' attributes MUST NOT be modified after being declared (draft-ietf-moq-msf-01 §5.3)` で、両者は文言で区別できる
5. 属性の比較は `comparable_attributes` で正規化して行う。同一性は履歴のキー (解決済み namespace, name) で確認するが、
   元の表現は一致するとは限らないため name / namespace / parentName は比較対象外にする。
   namespace は省略形と明示形で生の表現が異なり (どちらも同じ tuple を指す)、parentName は §5.2.33 (Parent name) により
   clone 以外では出現せず clone の解決後は常に `None` になる。
   `isLive=false` のときの `targetLatency` / `buffers` は §5.2.8 (Target latency) / §5.2.9 (Buffers) により無視されるため
   `None` とみなす (`Add` 経路は手組みの `MsfTrack` を正規化せず追加するため、比較側で正規化しないと結果が構築経路に依存する)。
   この正規化により、`isLive=false` のトラックでは `targetLatency` / `buffers` の有無だけが異なる再追加を受理する。
   実効的な属性が同じなら受理するという本 issue の範囲 (属性変更と isLive の逆行の検出) に対応する判断であり、
   これらの値そのものの変更検出は範囲外である。値まで含めて拒否するかは別 issue で扱う
   この正規化により、`isLive=false` のトラックでは `targetLatency` / `buffers` の有無だけが異なる再追加を受理する。実効的な属性が同じなら受理するという本 issue の範囲 (属性変更と isLive の逆行の検出) に対応する判断であり、これらの値そのものの変更検出は範囲外である。値まで含めて拒否するかは別 issue で扱う
6. 履歴の更新は `entry().or_insert()` とし、同じ tuple が再び削除されても最初に削除されたときの属性を保持する。正規化された属性が等しい再追加だけが受理されるため、直近の削除で履歴を上書きしても検証結果は変わらないが、公開フィールドとして観測できる履歴の値が変わるため、§5.3 が求める「宣言時の属性」を保持する側に倒している
7. 同一属性での remove → add は引き続き受理する。削除しても履歴は消えないため、delta update をまたいだ remove → add でも属性変更を検出できる
8. `MsfCatalog::apply_delta` の `# Errors` に新しい 2 条件 (属性変更 / isLive の逆行) を追記し、キー生成は `MsfCatalog::track_key` に一本化、操作名は private enum `ReaddOperation` で表現した

公開 API の変更:

- `MsfCatalog` に公開フィールド `removed_tracks` を追加した。構造体リテラルで `MsfCatalog` を構築している箇所は新フィールドが必要になる (破壊的変更、`CHANGES.md` の `[CHANGE]` に記載)
- `PartialEq` / `Debug` の比較対象に履歴が含まれる。JSON が同一でも remove の履歴が異なれば `MsfCatalog` は不一致になり、`Debug` には削除済みトラックも現れる (この旨を構造体 doc と `CHANGES.md` に明記した)。既存テストのうちカタログ全体を比較するものは、比較する両辺の履歴が同じ (通常は両方空) であるため影響しないことを `cargo test --workspace` で確認した

テスト:

- `tests/test_msf/delta_apply.rs` に 6 本追加した
  - `apply_delta_readd_with_changed_attribute_rejected`: remove → add で codec / label を変えた delta update が `InvalidCatalog` になり、失敗時にカタログが変わらない (copy-on-write) ことを固定する
  - `apply_delta_readd_by_clone_with_changed_attribute_rejected`: remove → clone で同じ (namespace, name) を別の親から再追加して属性が変わる delta update が `InvalidCatalog` になることを固定する
  - `apply_delta_readd_with_is_live_regression_rejected`: remove → add で isLive を false から true に戻す delta update が `InvalidCatalog` になり、文言が `isLive` を含み属性差分の文言と区別できることを固定する
  - `apply_delta_readd_attribute_change_across_deltas_rejected`: delta update をまたぐ remove → add でも属性変更が検出されることを固定する
  - `apply_delta_readd_with_same_attributes_succeeds`: 同一属性での remove → add が引き続き成功することを固定する
  - `apply_delta_readd_is_live_false_ignores_target_latency` / `apply_delta_readd_is_live_false_ignores_buffers`: `isLive=false` のトラックで `targetLatency` / `buffers` の有無だけが異なる再追加が成功することを固定する
  - `apply_delta_readd_attribute_change_with_inherited_namespace_rejected`: namespace を省略したトラックでも、継承した namespace で再追加の属性変更を検出することを固定する
  - `apply_delta_readd_with_explicit_namespace_matches_inherited`: 宣言は省略・remove は明示・再追加は省略という組み合わせで属性変更を検出する (履歴キーの解決に catalog namespace の継承が効いていることを固定する)
  - `apply_delta_readd_with_different_namespace_spelling_succeeds`: 明示 → 省略 / 省略 → 明示の両方向で、同一属性なら再追加が成功することを固定する (namespace の生表現差を属性変更とみなさない)
- 変異実験で検出力を確認した (履歴への記録を止める / isLive の検査を削除する / `isLive=false` の targetLatency と buffers の正規化をそれぞれ削除する / `track_key` の namespace 継承を削除する / `comparable_attributes` の name・namespace・parentName の正規化を削除する / 履歴の更新を `insert` に変える の各変異で対応するテストが失敗する。履歴の更新規則だけは、受理される再追加の正規化後属性が等しいため検証結果に現れないが、履歴の値は公開フィールドなので差は観測できる)
- 既存の `pbt/tests/prop_msf_delta.rs` の `add_then_remove_restores_tracks` は add → remove のみを扱うため影響せず、複数の seed で成功することを確認した

`CHANGES.md` の `## develop` に `[CHANGE]` (公開フィールド追加と `PartialEq` / `Debug` の意味変化) と `[FIX]` を追加し、`docs/IMPLEMENTATION.md` の MSF 実装状況にも `removedTracks` と新しい検証規則を追記した。

# MSF の delta update で同一 Track の属性変更を検出しない

- Created: 2026-09-21
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-delta-track-attribute-change
- Polished: {YYYY-MM-DD}

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
- `MsfDeltaOperation::Add` は `find_track_index` で現在の `tracks` / `publish_tracks` との名前重複を検査して `tracks` へ push する
- `MsfDeltaOperation::Clone` は親を `MsfCloneTrack::into_track` で解決したうえで、同じく現在の名前重複だけを検査して push する
- `validate_after_delta` は名前の一意性、`initRef`、同一 group の `targetLatency` / `buffers` 一致を検証するだけで、削除済み Track を参照しない

`MsfCatalog` には削除済み Track の履歴を持つフィールドが無い。このため次の delta update がすべて成功する。

- remove → add で `label` / `codec` / `targetLatency` などの属性を変更する
- remove → clone で解放された名前を再利用し、親から継承した属性で元と異なる属性にする
- remove → add で `isLive` を false から true にする (§5.2.7 違反)

remove → add の属性変更を検証する既存テストは無い。`pbt/tests/prop_msf_delta.rs` の `add_then_remove_restores_tracks` は add → remove の可逆性だけを検証している。

## 設計方針

削除した Track の属性を履歴として `MsfCatalog` に保持し、同じ (namespace, name) が再追加されたときに属性変更と isLive の逆行を検出する。

- 履歴は `MsfCatalog` のフィールドとして持つ。1 回の `apply_delta` の呼び出し内だけで判断すると、delta update をまたいだ remove → add を検出できない
- キーは `MsfCatalog::find_track_index` と同じ規則で解決した (namespace, name) とする。namespace 省略時は catalog の namespace を継承したものとして比較する (draft-ietf-moq-msf-01 §5.2.2 (Track namespace))
- 値は削除された `MsfTrack` を 1 件保持する。属性変更の検出には属性集合そのものが必要であり、`MsfTrack` は `Clone` と `PartialEq` を実装済みのため、エンコード文字列やハッシュへ落とすより取りこぼしが無い
- 履歴は (namespace, name) ごとに 1 エントリとし、同じ tuple が再追加されても削除前の属性を保持し続ける。エントリ数は catalog が扱う tuple 数と同じ桁に収まる
- 再追加時は name と namespace を除く属性を比較し、差分があれば `InvalidCatalog` を返す。Add と Clone の両経路を対象にする
- 比較は実効的な属性で行う。`isLive=false` のとき `targetLatency` / `buffers` は §5.2.8 (Target latency) / §5.2.9 (Buffers) により無視され、`MsfCloneTrack::into_track` と decoder は `None` へ正規化するため、`isLive=false` の Track で `targetLatency` の有無だけが異なる再追加は属性変更として扱わない
- `isLive` の false → true は §5.2.7 の独立した MUST のため、違反した MUST を特定できる専用メッセージで `InvalidCatalog` を返す。属性比較でも検出できるが、エラー理由を区別できるようにする
- 同一属性での remove → add は本 issue では引き続き受理する。§5.3 の「未宣言の Track の追加」という文言だけを見れば拒否する解釈もあるが、本 issue は属性変更と isLive の逆行の検出に絞る

`MsfCatalog` は全フィールドが public で、`tests/` 配下の構造体リテラルからも構築されている。フィールドを追加するときは `MsfCatalog::new` と `decode_full_catalog` に加えてこれらのリテラルを更新する。履歴が `PartialEq` の比較対象に含まれるため、カタログ全体を `assert_eq!` で比較する既存テストへの影響も確認する。

subscriber が delta update を継続受信して適用する仕組みは [issues/pending/0006](../issues/pending/0006-add-msf-catalog-subscribe.md) が扱う。本 issue は `MsfCatalog::apply_delta` の検証のみを対象とする。

## 完了条件

- remove → add で `label` / `codec` などの属性を変更した delta update が `InvalidCatalog` になるテストが `tests/test_msf/delta_apply.rs` に追加されていること
- remove → clone で同じ (namespace, name) を再追加し属性が変わる delta update が `InvalidCatalog` になるテストが追加されていること
- remove → add で `isLive` を false から true にする delta update が `InvalidCatalog` になるテストが追加されていること
- delta update をまたぐ remove → add でも属性変更が検出されるテストが追加されていること (1 回の delta に閉じない)
- 同一属性での remove → add が引き続き成功するテストが追加されていること
- `isLive=false` の Track で `targetLatency` の有無だけが異なる再追加が成功するテストが追加されていること
- `pbt/tests/prop_msf_delta.rs` の `add_then_remove_restores_tracks` と `tests/test_msf/delta_apply.rs` の既存テストがすべて成功すること
- `make test` (`cargo test --workspace`) と `make clippy` と `make fmt` が通ること

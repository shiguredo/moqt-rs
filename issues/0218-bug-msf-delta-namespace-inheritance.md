# カタログ delta の適用で catalog namespace の継承を成立させる

- Created: 2026-10-09
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-msf-delta-namespace-inheritance
- Polished: 2026-10-10
- Updated: 2026-10-10

## 目的

draft-ietf-moq-msf-01 §5.2.2 (Track namespace) は「If it is not declared within a track, then each
track MUST inherit the namespace of the catalog track.」と MUST を定める。§5.2.34 (Parent namespace)
も clone の parentNamespace 省略時に catalog の namespace を仮定すると定める。delta update の
remove / clone は track object の namespace を省略できる (§5.1.6 (Delta update) は remove の
track object が Track Namespace を MAY で含むと定める) ため、適用側は catalog track の
namespace を継承先として解決できなければならない。現状の moq-sub は継承先を渡していないため、
§5.6.1 のように track が namespace を宣言するカタログへ、§5.6.5 のように namespace を
省略した delta を適用できない。track が namespace も省略するカタログでは、継承先が None
扱いになり §5.2.3 の一意性検査と §5.3 の属性不変検査が正しく働かない。

## 現状

- `examples/moq-sub/src/catalog.rs` の `CatalogState::apply` は
  `MsfCatalog::apply_delta(delta, None)` を呼ぶ。第 2 引数 `catalog_namespace` は
  「namespace 省略時の継承先」であり、`None` は「catalog の namespace が不明」を意味する。
- `src/msf.rs` の `MsfCatalog::find_track_index` は
  `t.namespace.as_deref().or(catalog_namespace)` と比較するため、`None` では「namespace を宣言した
  track」と「namespace を省略した delta の操作対象」が一致しない。
- 結果として、namespace を宣言した track を namespace を省略した remove / clone で操作すると
  `delta remove: track 'video' not found` のような `InvalidCatalog` になり、`CatalogState::apply`
  が `Error::Other` を返す。`MsfCatalog::apply_delta` は複製へ適用してから差し替えるため、
  カタログは変更されないまま delta の変更が失われる。
- 起動経路 (`examples/moq-sub/src/pipeline.rs` の `receive_catalog` → `apply_catalog_object`) では
  `?` が伝播して example が終了する。
- §5.2.3 (Track name) の (namespace, name) 一意性検査と §5.3 (Delta updates) の属性不変検査も、
  namespace を省略した track に対しては継承が解決できず無効化される。
- 一次資料には namespace を宣言する例と省略する例の両方がある。§5.6.1 (Time-aligned Audio/Video
  Tracks with single quality) は track ごとに `namespace` を宣言し、§5.6.5 (Delta update removing
  tracks) は `{"op":"remove","tracks":[{"name":"video"},{"name":"slides"}]}` で namespace を省略する。
- catalog track の namespace は example が保持している。`examples/moq-sub/src/pipeline.rs` の
  `run` の `namespace` (CLI または MSF fragment の track-identifier 由来) がそれにあたり、
  `examples/moq-pub/src/pipeline.rs` も `serialize_namespace` の結果を track の `namespace` に書く。

## 設計方針

- `CatalogState` が catalog track の namespace を保持し、`apply_delta` の第 2 引数へ渡す。
  比較対象は catalog JSON の `namespace` フィールドと同じ表現であるため、
  `shiguredo_moqt::name::serialize_namespace` で作る draft-ietf-moq-transport-22 §8.8
  (Representing Namespace and Track Names。MSF では §11.1.2 に再掲) の表現の文字列を使う。
- namespace は `CatalogState::new` の引数で受け取り、`CatalogState` が保持する。
  起動経路の `receive_catalog` と継続受信は同じ `CatalogState` を共有しているため、
  `apply` が保持した値を `apply_delta` へ渡せば両経路に同じ値が届く。
- catalog track の namespace は接続時に確定しているため、`None` を渡す経路を残さない。
- 継承の解決結果を使って §5.2.3 の一意性検査と §5.3 の属性不変検査が働くことを確認する。

## 完了条件

- namespace を省略した remove / clone を含む delta を適用できること
  (`examples/moq-sub/src/catalog.rs` のテストで固定する)。
- namespace を宣言した track と namespace を省略した操作が一致すること、namespace が異なる
  track を誤って削除しないことの両方をテストで確認すること。
- §5.6.1 の例にならい track に namespace を宣言したカタログへ、§5.6.5 の例にならい
  namespace を省略した remove を含む delta を適用できること。track 名は両者で揃える
  (§5.6.1 の例は 1080p-video / audio、§5.6.5 の例は video / slides で一致しないため)。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` /
  `cargo fmt --all -- --check` が通ること。

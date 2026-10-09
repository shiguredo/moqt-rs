# MSF catalog track の購読と delta 継続受信に対応する

- Created: 2026-09-09
- Completed: 2026-10-09
- Branch: feature/add-msf-catalog-subscribe
- Polished: {YYYY-MM-DD}

## pending にした理由

MSF-01 §5 (Catalog) は「Subscribers accessing the catalog MUST use SUBSCRIBE with a Joining FETCH (offset = 0) in order to obtain the latest complete catalog along with all subsequent catalog objects, including delta updates, that follow.」と規定する。
しかし MSF-01 は [MoQTransport] として draft-ietf-moq-transport-18 を参照しており、本リポジトリが対象とする draft-ietf-moq-transport-20 では Joining FETCH が廃止され fill fetch stream に置き換わっている (transport-20 の変更履歴「Replace Joining FETCH with fill fetch streams」)。

MSF-01 の MUST を transport-20 上でどう表現するか (SUBSCRIBE + fill fetch stream か、SUBSCRIBE のみか) が仕様間で未確定であり、誤った写像で実装すると相互運用を損なうため保留する。加えて以下が必要である。

- catalog track を購読し続け、新しい Group の独立カタログと delta を受信・適用する状態管理
- §5 の「最初の Object (Object ID 0) は独立カタログ、以降 (Object ID >= 1) は delta」「独立カタログは新しい Group の先頭に置く」の検証
- catalog は sub-group 0 にマップする (§5) 規則の扱い

MSF-01 と transport-20 の対応が確定した時点で本 issue を `issues/` へ戻して対応する。

## 目的

MSF-01 §5 (Catalog) に従い、subscriber が catalog track を購読して最新の完全カタログと後続の delta update を継続的に受信・適用できるようにする。

## 現状

- `examples/moq-sub/src/pipeline.rs` の `receive_catalog` は固定 range の standalone FETCH で 1 回だけ取得する
- 取得した全 Object を読み、`MsfCatalog::apply_delta` で delta を適用するようにはしたが、購読を継続しないため新しい Group の delta は受信しない
- ライブラリには catalog track 固有の購読・delta 適用状態を扱う型は無い (`MsfCatalog::apply_delta` は単発適用のみ)
- `src/session/` は汎用の SUBSCRIBE / FETCH を扱うが、catalog の Group / Object 規則は関知しない

## 設計方針

- transport-20 での Joining FETCH 相当 (fill fetch stream) の扱いを確定し、catalog track の購読方法を決める
- 受信した catalog Object を順に適用する状態 (直前の独立カタログ、Group 境界) をライブラリの型または example のどちらに持たせるかを確定する
- §5 の「Object ID 0 = 独立カタログ」「独立カタログは新 Group の先頭」を検証するか、application 層の責務とするかを確定する

## 完了条件

- catalog track を購読して最初の独立カタログと後続 delta を継続受信できること
- §5 の独立カタログ / delta の配置規則を検証または明示的に扱えること
- `tests/` / `pbt/` または example の動作確認で裏付けられていること

## 解決方法

- catalog track を SUBSCRIBE (Next Object の Location Filter) し、SUBSCRIBE_OK の LARGEST_OBJECT が示す Group の先頭 Object から FETCH する形にした (`examples/moq-sub/src/pipeline.rs` の `catalog_fetch_filter` / `receive_catalog`)
  - MSF-01 §5 の "SUBSCRIBE with a Joining FETCH (offset = 0)" は、Joining FETCH が廃止された draft-ietf-moq-transport-22 §3.5.1 の購読パターンで表す。Group ID を 0 と仮定しない (draft-ietf-moq-msf-01 §6.1)
  - LARGEST_OBJECT が未広告のときは FETCH を発行しない (同 §3.2 は Object が 1 つも無い track への FETCH に INVALID_RANGE を MUST とする)。購読で届く最初の独立カタログを待つ
  - Group ID が Unix epoch ミリ秒から始まる publisher の catalog も取得できる
- 受信した Object を `examples/moq-sub/src/catalog.rs` の `CatalogState` が MSF-01 §5 の配置規則で適用する
  - Group の最初の Object (Object ID 0) は独立したカタログとして置き換える
  - 同じ Group の Object ID >= 1 は delta update として `MsfCatalog::apply_delta` で適用する
  - 最新 Group より前の Object、独立したカタログを持たない Group の delta、適用済みと重複する Location は適用しない
- 購読は維持し、以降に配られるカタログも main ループが同じ規則で適用し続ける (購読で届く Object は stream task からチャネルで main ループへ渡す)
- `CatalogState` の単体テストで §5 の配置規則 (置き換え / delta 適用 / 最新 Group より前の無視 / 重複の無視 / 独立カタログ無しの delta の無視) を固定した
- 実 relay を介した E2E で、Group ID を Unix epoch ミリ秒から始める publisher の catalog を取得して映像が届くことを確認した

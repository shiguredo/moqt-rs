# MSF catalog track の購読と delta 継続受信に対応する

- Created: 2026-09-09
- Completed: {YYYY-MM-DD}
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

- `examples/moqt-subscriber/src/pipeline.rs` の `receive_catalog` は固定 range の standalone FETCH で 1 回だけ取得する
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

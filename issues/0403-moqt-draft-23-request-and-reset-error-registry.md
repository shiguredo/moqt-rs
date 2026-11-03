# 要求・配送終了・stream reset のエラーコード集合を更新する

- Created: 2026-11-03
- Completed: {YYYY-MM-DD}
- Branch: feature/change-moqt-draft-23-request-and-reset-error-registry
- Polished: {YYYY-MM-DD}

## 目的

MOQT draft 22 から draft 23 への変更に追従する。
根拠は [draft-ietf-moq-transport-23 §9.9、§12.3、§12.4、§12.5、§16.12.2 〜 §16.12.4](../refs/moq/draft-ietf-moq-transport-23.txt)。

## 現状

`src/error.rs` は REQUEST_PREFIX_OVERLAP と STREAM_UNKNOWN_OBJECT_STATUS を既知コードに含む一方、UNAVAILABLE と PUBLISH_DONE の EXPIRED_AUTH_TOKEN がない。
`src/session/types.rs` の `DataStreamResetReason::UnknownObjectStatus` は削除されるコードを送出できる。

## 設計方針

REQUEST_ERROR の UNAVAILABLE (`0x0A`) と PUBLISH_DONE の EXPIRED_AUTH_TOKEN (`0x07`) を追加し、PREFIX_OVERLAP (`0x30`) と stream reset の UNKNOWN_OBJECT_STATUS (`0x06`) を現行定義から外す。
未知 reset / request code の正規化規則に従って旧値の受信を扱い、Unknown の範囲は reset で中断せず End of Unknown Range で続行する使い方へ移す。
認可失効で subscription を終えるアプリが status `0x07` を選べるようにする。

## 完了条件

- 新コードを送受信・通知しても INTERNAL_ERROR へ正規化されないこと。
- 旧コードを既知として送出する API がなく、受信時は未知コードの既存規則に従うこと。
- Unknown Range を含む FETCH が既知の残りを配送できること。
- セッション終了コードの分類移行とは分離し、対応する公開 enum・説明・テストを揃えること。

## 依存・関連

- 関連: [0395](0395-moqt-draft-23-fetch-gone-range.md)

# 要求・配送終了・stream reset のエラーコード集合を更新する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

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

## 解決方法

`src/error.rs` のコード集合を draft-ietf-moq-transport-23 §12.3 / §16.12.2、§12.4 / §16.12.3、§12.5 / §16.12.4 に揃えた。

- REQUEST_ERROR に `REQUEST_UNAVAILABLE` (`0x0A`) を追加し、既知コード表 `KNOWN_REQUEST_ERROR_CODES` に加えた。§12.3 の Retry Interval を使う SHOULD を doc コメントに明記した
- PUBLISH_DONE に `PUBLISH_DONE_EXPIRED_AUTH_TOKEN` (`0x7`) を追加し、`KNOWN_PUBLISH_DONE_CODES` に加えた。§9.9 の「認可したトークンの失効で購読を終える場合は EXPIRED_AUTH_TOKEN を送る」SHOULD と §12.4 の定義を doc コメントに明記した
- REQUEST_ERROR の `REQUEST_PREFIX_OVERLAP` (`0x30`) と Stream Reset の `STREAM_UNKNOWN_OBJECT_STATUS` (`0x6`) を定数と既知コード表から削除した
- 削除値の受信は §13 (Grease) の未知コード規則に従い、`normalize_request_error_code` / `normalize_stream_reset_code` が各文脈の `INTERNAL_ERROR` に正規化する
- `src/session/types.rs` の `DataStreamResetReason` から `UnknownObjectStatus` variant を削除した
- Unknown な範囲は End of Unknown Range (`0x10C`、§11.4.2 (End of Range)) で示して既知 Object の配送を続ける (§3.2.2 (Gaps in a Fetch Stream)) ことを enum の doc コメントに明記した
- `docs/moqt.md` に「要求・配送終了・stream reset のエラーコード」節を追加し、`skills/shiguredo-moqt/SKILL.md` に `DataStreamResetReason` の説明と `PUBLISH_DONE_EXPIRED_AUTH_TOKEN` の使い方を追記した
- セッション終了コードの分類 (§12.2 / §16.12.1) は変更していない

`crates/tokio-moq` / `crates/moq-sub` / `crates/moq-pub` に削除値への参照が無いことを確認したため、これらの crate のコード変更は不要だった。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| 新コードを送受信・通知しても INTERNAL_ERROR へ正規化されないこと | `tests/test_error.rs` の `added_codes_are_not_normalized_to_internal_error`、`request_error_unavailable_is_not_normalized`、`publish_done_expired_auth_token_is_not_normalized`、PBT の `defined_codes_are_kept_in_each_context` |
| 旧コードを既知として送出する API がなく、受信時は未知コードの既存規則に従うこと | `UnknownObjectStatus` / `REQUEST_PREFIX_OVERLAP` / `STREAM_UNKNOWN_OBJECT_STATUS` を定義から削除した。`tests/test_error.rs` の `removed_codes_are_normalized_to_internal_error` が 0x30 / 0x6 の正規化を固定する |
| Unknown Range を含む FETCH が既知の残りを配送できること | `tests/test_session/fetch/unified.rs` の `fetch_continues_known_objects_after_end_of_unknown_range` が、reset / STOP_SENDING を発行せず後続の既知 Object を受理することを固定する |
| セッション終了コードの分類移行とは分離し、対応する公開 enum・説明・テストを揃えること | §12.2 / §16.12.1 の表と分類は無変更。`DataStreamResetReason` の doc コメント、`docs/moqt.md`、`skills/shiguredo-moqt/SKILL.md`、テストを更新した |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo test --workspace` / `RUSTDOCFLAGS="-D warnings" cargo doc -p shiguredo_moqt --no-deps` はすべて成功した。

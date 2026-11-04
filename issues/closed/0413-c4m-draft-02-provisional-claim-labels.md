# moqt / moqt-reval の暫定 claim label を更新する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §8、Appendix A、A.5](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m.rs` の `CLAIM_MOQT` / `CLAIM_MOQT_REVAL` は draft 01 のベクタ由来の 327 / 328 を採用する。
`src/c4m/cat.rs` の typed key 判定・decode / encode もこの定数に依存する。
draft 02 は IANA の割当が未確定と明記し、ベクタでは Private Use の -65537 / -65538 を使う。

## 設計方針

新しいベクタの暫定 label を明示的に扱うよう、定数と CAT の処理を揃える。
Private Use の値を正式な IANA 割当として説明しない。
旧 327 / 328 を新 label の無条件 alias として解釈せず、旧版を維持する場合は版を明確に分ける。
moqt claim value の map 化とは別の変更として検証する。

## 完了条件

- 新 label の token で scope と revalidation 値を型付きに読み書きできること。
- `is_typed_claim_key` と raw claim の重複検出が新 label に一致すること。
- 未割当であることと採用する暫定値が公開説明・テストに明記されること。

## 依存・関連

- 関連: 0412 (完了済み) / [0419](0419-c4m-draft-02-test-vectors.md)
- 関連: [0420](0420-c4m-draft-02-documentation-and-coverage.md)

## 差分の確認範囲

- `CLAIM_MOQT` / `CLAIM_MOQT_REVAL` を draft-02 §A.5 の Private Use 暫定値 (-65537 /
  -65538) へ変更し、IANA 未割当であること・確定時に置き換えることを doc に明記
- `is_typed_claim_key`、`CatClaims` の decode / encode、raw claim の重複検出を新 label に
  揃え、旧 draft-01 ベクタの 327 / 328 は alias にせず raw に保持
- `SCOPE_VECTORS` の claim label と compact トークンの HMAC を新 label に追随
- 単体テストの追加

docs / README / skills の C4M draft-02 記述の更新は 0420 で行う。c4m モジュール doc と各
フィールド doc に残る draft-01 の節参照も 0420 でまとめて更新する。公式ベクタ (label の正式
割当・COSE_Mac0 形式) への完全一致は 0419 で行う。action 番号の draft-02 追随は後続 issue で
行う。CHANGES.md は CODEBASE の方針により更新しない。`moqt` claim の値 (map 形式) 自体は
0412 で対応済みであり、本変更は claim label と CAT の typed key 判定に限定している。

## 解決方法

`moqt` / `moqt-reval` の暫定 claim label を draft-02 付録 A.5 の Private Use の値へ変更し、
CAT の typed key 判定と raw claim の扱いを揃えた。

- `src/c4m.rs` の `CLAIM_MOQT` を -65537、`CLAIM_MOQT_REVAL` を -65538 に変更した。
  draft-02 §8 Table 2 は Key を `TBD_MOQT (1+2)` とし、§2.2 は `TBD_MOQT_REVAL` とするため
  IANA の割当は未確定であり、§A.5 のベクタは Private Use (RFC 8392 §9.1.1) の値を
  プレースホルダとして使う。この暫定値は割当確定時に置き換える
- 旧 draft-01 のベクタが使う 327 / 328 は新 label の alias として解釈しない。327 / 328 を
  キーに持つ claim は未知の claim として `CatClaims::raw` に保持し、型付きフィールド
  (`moqt` / `moqt_reval`) へは写さない。旧 label の token は decode 自体は成功するが
  `moqt` が `None` のままなので認可されない (fail-closed)。公開定数の値が変わるため、
  旧 label を前提にした利用者には破壊的変更である (CODEBASE の方針に沿う意図的な変更)
- `src/c4m/cat.rs` の `is_typed_claim_key`、`CatClaims::decode` / `encode`、raw claim の
  重複検出は定数参照のまま新 label に追随する。同じ claim が型付きフィールドと raw の
  両方に現れた場合は従来どおり `DuplicateClaim` で拒否する
- `tests/test_c4m/vectors.rs` の `SCOPE_VECTORS` は claim label を -65537 / -65538 に更新し、
  A.1 の HMAC 鍵で compact トークンの署名を再計算した。`moqt-reval` の値は draft-02 §A.5 の
  整数 300 に合わせ、compact の alg は -4 のままである
- テストを追加した: `src/c4m/cat.rs` の定数 / `is_typed_claim_key` の検証、新 label の
  raw 重複検出、旧 label の raw 保持、encode / decode の往復、新 label の実 token から
  scope 認可と revalidation 値が読めること

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| 新 label の token で scope と revalidation 値を型付きに読み書きできること | `tests/test_c4m/moqt.rs` の `token_with_provisional_claim_labels_has_typed_claims`、`tests/test_c4m/cat.rs` の `provisional_*` / `compact_round_trip_with_hmac` |
| `is_typed_claim_key` と raw claim の重複検出が新 label に一致すること | `src/c4m/cat.rs` の `typed_claim_keys_include_provisional_moqt_labels`、`tests/test_c4m/cat.rs` の `provisional_moqt_claim_labels_are_rejected_in_raw` / `legacy_moqt_claim_labels_survive_token_round_trip_as_raw` |
| 未割当であることと採用する暫定値が公開説明・テストに明記されること | `CLAIM_MOQT` / `CLAIM_MOQT_REVAL` の rustdoc (IANA 未割当、付録 A の Private Use プレースホルダ、確定時置換、alias にしない)、`CatClaims` / `raw` / `is_typed_claim_key` の doc、上記テスト。ユーザーガイド `docs/c4m.md` の旧 label 表記は 0420 で更新する |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test -p shiguredo_moqt` / `cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` /
`cargo test -p pbt` / `cargo test -p moq-sub` / `cargo test -p tokio-moq --lib with_auth_parameters` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` はすべて成功した。

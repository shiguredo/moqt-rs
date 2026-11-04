# C4M の Appendix A ベクタを draft 02 に更新する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 Appendix A.1 〜A.7](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`tests/test_c4m/vectors.rs` は draft 01 の付録を定数化し、compact 3 分割 token、HMAC alg=-4、旧 moqt label と array、cnf key 3、旧 CBOR の CAT claim 例を使う。
draft 02 は COSE_Mac0 / COSE_Sign1 全体、alg=5、cnf jkt=323、Private Use label と map を使う新しい署名付きベクタへ更新している。

## 設計方針

新しい payload_cbor_hex / cose_hex / cose_b64 を定義元から採り込み、COSE の保護領域・MAC / 署名を独立した期待値で検証する。
改行とインデントの unfolding 規則を守り、claims_edn を独自の JSON claim 表現として解釈しない。
catv / catu / catnip / catalpn などの修正と異常系も確認する。
A.5 の `moqt_admin_wildcard` は actions が 0〜8、action 0 の期待値が true のままで、§2.1 Table 1 / 0414 の既知 action 1〜9 と一致しない。公式の payload / COSE / MAC と元の期待値は保持し、この不整合を明記して action 0 の認可期待値を通常の既知 action の検証から分離する。
action 0 は `MoqtAction::from_key` で未対応になることを固定し、SETUP の alias に戻さない。同じ公式 token に含まれる既知 action は通常どおり検証し、含まれない `SubscribeTracks=9` は認可しない。9 の許可は公式ベクタを改変せず、別の補助 fixture で検証する。
A.6 は OR / NOR / AND の label がすべて暫定の 0 で、wire だけでは演算子を区別できない。元の CBOR 構造 / COSE / MAC の確認に限定し、Composite の評価実装は追加しない。raw claim として保持・署名検証できることを Composite の受理・認可成功と混同せず、型付き scope 認可では許可にならないことを固定する。
旧 JWT / compact 汎用 API のテストを残す場合は、draft 02 の公式ベクタと混ぜない。

## 完了条件

- 新付録の token・payload・MAC / 署名を実暗号で検証し、自己生成値だけの roundtrip にしないこと。
- 改変 MAC、誤った鍵、形式 / algorithm の不一致、時刻・audience の異常系を新しい入力で検証すること。
- moqt map と暫定 label の対応を先行させ、ベクタの差を実装に都合よく書き換えないこと。
- A.5 の action 0 の不整合と A.6 の暫定 Composite を通常の認可成功ケースから分離し、公式バイト列・MAC / 署名を保持すること。
- 0356 の一般的な PBT / fuzz 改善は別課題として保ち、本 issue は一次資料ベクタの追従に絞ること。

## 依存・関連

- 先行: [0412](0412-c4m-draft-02-moqt-claim-map.md)
- 先行: [0413](0413-c4m-draft-02-provisional-claim-labels.md)
- 先行: [0414](0414-c4m-draft-02-action-codes.md)
- 関連: [0356](0356-test-c4m-pbt-model-fuzz-gaps.md)

## 差分の確認範囲

- `tests/test_c4m/vectors_draft02.rs` に draft-02 付録 A.1 〜 A.7 の公式ベクタを追加
- `tests/test_c4m/draft02.rs` に公式バイト列を実暗号で検証するテストを追加
- `tests/test_c4m.rs` のモジュール登録とモジュール doc の更新
- `tests/test_c4m/vectors.rs` は doc と、feature なしビルドの expect(dead_code) 属性を更新
  (draft-01 の compact ベクタは残置)
- `tests/test_c4m/dpop/cwt/verification.rs` の `use super::*` を aws-lc-rs feature 時だけに
  する gate を追加 (feature なし clippy の警告を出さないため)

src の実装は変更しない。draft-01 の compact 形式 / JWT 形式の汎用 API のテストは残し、
draft-02 の公式 COSE ベクタとは混ぜない。0356 の PBT / fuzz 改善は対象外。docs / README /
skills の更新は 0420 で行う。CHANGES.md は CODEBASE の方針により更新しない。

## 解決方法

draft-02 付録 A.1 〜 A.7 の公式テストベクタを追加し、公式バイト列を直接使って検証する
テストを追加した。

- `tests/test_c4m/vectors_draft02.rs` に、A.1 の鍵 (HMAC / ES256)、A.2 の claims 8 件、
  A.3 の token 3 件、A.4 の DPoP 3 件、A.5 の scope 5 件、A.6 の Composite 4 件、
  A.7 の validation 8 件を、規範的な payload / COSE / MAC / 署名と検証入力について
  定義元の値のまま定数化した。hex / base64url / token の折り返しは付録 A 冒頭の
  unfolding 規則に従って展開している。説明用フィールド (`description` と A.5 の
  `moqt_scopes`) は保持しない
- `tests/test_c4m/draft02.rs` に、公式の payload / COSE / MAC / 署名を直接デコードし、
  aws-lc-rs の実暗号 (HMAC-SHA256 / ES256) で検証するテストを追加した。
  改ざん MAC、誤った鍵、COSE タグとアルゴリズムの不一致、時刻 / issuer / audience の
  異常系も公式ベクタで固定する
- A.5 の `moqt_admin_wildcard` は actions 0〜8 と action 0 の期待値 true を公式のまま
  保持する。action 0 は `MoqtAction::from_key` で未対応のまま (SETUP の alias に戻さない)
  とし、action 1〜8 は通常どおり認可、`SubscribeTracks=9` は認可されないことを固定した。
  9 の許可は公式ベクタを改変せず、既知バイト列の補助 fixture で検証する
- A.6 の Composite は全 claim key が暫定 0 で wire から演算子を区別できないため、
  decode・MAC 検証・raw 保持・公式 payload との一致に限定し、`CatClaims::authorize` では
  既知 action のどれも許可にならないことを固定した。Composite の評価は実装しない
- A.7 の `invalid_algorithm_mismatch` は公式の期待が `InvalidTokenFormat` だが、本実装は
  `CatError::AlgorithmMismatch` を返す。公式値は定数に保持し、テストでは実装の挙動を
  コメント付きで固定した。`reference_time` を持たない A.7 ベクタは署名 / 構造の異常を
  見るため claim の時刻検証を行わない
- draft-01 の compact ベクタは compact / JWT 汎用 API 用に残し、draft-02 の公式ベクタと
  は別モジュール・別定数名で分離した。両者の claim set と A.1 の鍵の同期テストも追加した

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| 新付録の token・payload・MAC / 署名を実暗号で検証し、自己生成値だけの roundtrip にしないこと | `tests/test_c4m/draft02.rs` の `*_verify_with_real_crypto` (token / dpop / scope / composite / validation) |
| 改変 MAC、誤った鍵、形式 / algorithm の不一致、時刻・audience の異常系を新しい入力で検証すること | 同 `tampered_mac_and_wrong_key_are_rejected` / `cose_mac0_tag_with_mac_algorithm_is_rejected_as_sign1` / `validation_vectors_decode_and_validate` / `validation_vectors_verify_with_real_crypto` |
| moqt map と暫定 label の対応を先行させ、ベクタの差を実装に都合よく書き換えないこと | 同 `scope_vectors_authorize_from_cose_tokens` (公式 payload の再エンコード一致) / `token_vectors_decode_cose` / `dpop_vectors_decode_cose` |
| A.5 の action 0 の不整合と A.6 の暫定 Composite を通常の認可成功ケースから分離し、公式バイト列・MAC / 署名を保持すること | 同 `scope_admin_wildcard_action_zero_is_not_mapped` / `subscribe_tracks_is_authorized_by_auxiliary_fixture` / `composite_vectors_are_kept_as_raw_claims` |
| 0356 の一般的な PBT / fuzz 改善は別課題として保ち、本 issue は一次資料ベクタの追従に絞ること | PBT / fuzz は変更していない (`pbt/` と `fuzz/` の差分なし) |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --test test_c4m -- -D warnings` /
`cargo clippy --workspace --all-targets -- -D warnings` / `cargo test -p shiguredo_moqt` /
`cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` / `cargo test -p pbt` /
`cargo test -p moq-sub` / `cargo test -p tokio-moq --lib with_auth_parameters` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` /
`cargo check --manifest-path fuzz/Cargo.toml --bins` はすべて成功した。

# moqt claim を map 化し未知のキーを fail-closed で扱う

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §2.1、§2.1.1](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m.rs` の `MoqtClaim::decode` / `encode` は scopes の配列を claim value そのものとして扱う。
`src/c4m/cat.rs` の `CatClaims::decode` もこの形へ直接委譲するため、draft 02 の map を読み書きできない。

## 設計方針

moqt claim を CBOR map とし、キー 0 に scopes 配列を格納する。
未知の map key を見つけた場合、制限を黙って無視せず、その claim を含む claim set を受理不可とする。
最上位 claim set なら token を拒否する。
Composite の分岐評価を実装していない現在の API で、未知キーを取り除いて認可する処理は追加しない。
複数 scopes 自体の OR と、Composite claim set の OR を混同しない。

## 完了条件

- 新しい map の既知バイト列で encode / decode / authorize が一致すること。
- キー 0 の欠落・型不一致・空 scopes と、未知キー付き claim を検証すること。
- 未知の制限が加わった token を認可せず、旧 array-only の値を新形式として誤受理しないこと。

## 依存・関連

- 関連: [0413](0413-c4m-draft-02-provisional-claim-labels.md)
- 関連: [0419](0419-c4m-draft-02-test-vectors.md)
- 関連: [0420](0420-c4m-draft-02-documentation-and-coverage.md)

## 差分の確認範囲

- `moqt` claim の CBOR map 化 (キー 0 = scopes 配列)、旧 array-only の拒否、未知キーの
  fail-closed 拒否
- `C4mError` への `MissingScopesKey` / `DuplicateScopesKey` / `UnknownMoqtClaimKey(i64)` の
  追加
- `SCOPE_VECTORS` の `moqt` claim 値を draft-02 の map 形式へ更新し、token / HMAC を再計算
- 単体テスト / PBT の追加
- `crates/tokio-moq` のテスト fixture の doc コメントを draft-02 §2.1 に更新

`moqt` claim 以外の C4M 部分 (claim label、`moqt-reval`、action 番号、`catdpop` など) の
draft-02 追随は後続 issue で行う。MOQT の action 番号の意味は変更していない。

非対象と確認した範囲:

- docs / README / skills の C4M draft-02 記述の更新は 0420 で行う
  (`docs/c4m.md` の旧 array 形式の記述は、この変更後はコードと一時的に不一致になる)。
  CHANGES.md は CODEBASE の方針により更新しない
- draft-02 付録 A.5 の公式ベクタへの置換 (claim label の -65537 化を含む) は後続で行う
- claim label の変更と `moqt-reval` の draft-02 表現への追随は後続で行う
- compact トークンの protected header の alg は -4 のままであり、draft-02 付録 A.5 の
  公式値を入れる後続対応で置換する
- action 番号 (SETUP=1 / SUBSCRIBE_TRACKS=9 / 0 の廃止) の追随は後続で行う
- `tests/test_c4m/moqt.rs` のサブモジュール分割は、公式ベクタを追加する時点で行う

## 解決方法

moqt claim を draft-02 §2.1 の CBOR map 形式へ変更し、未知キーを fail-closed で拒否する
ようにした。

- `src/c4m.rs` の `MoqtClaim::decode` は CBOR map のみを受理し、キー 0 (`MOQT_SCOPES_KEY`)
  の scopes 配列を要求する。キー 0 の欠落は `MissingScopesKey`、値が配列でない場合は
  `UnexpectedType("moqt-scopes")`、空 scopes は `EmptyScopes`、キー 0 の重複は
  `DuplicateScopesKey`、i64 で表せないものを含め解釈できないキーは
  `UnexpectedType("moqt claim key")`、キー 0 以外のキーは `UnknownMoqtClaimKey(i64)` で
  拒否する
- 旧 draft-01 の array-only 値は map でないため `UnexpectedType("moqt claim")` で拒否し、
  新形式として誤受理しない
- `MoqtClaim::encode` はキー 0 に scopes 配列を持つ CBOR map を返す
- `C4mError` に `MissingScopesKey` / `DuplicateScopesKey` / `UnknownMoqtClaimKey(i64)` を
  追加した。既存の利用箇所は `C4mError` を `CatError::C4m` に包んで伝播する `From` 実装と
  Display のみで動作は変わらないが、
  公開 enum を網羅 match する利用者には variant 追加がコンパイル上の破壊的変更になる。
  CODEBASE の「破壊的変更は積極的に行う」方針に沿った意図的な変更である
- `CatClaims::decode` は `MoqtClaim::decode` に委譲するため、未知キー付き claim を含む
  claim set は decode 時点で受理不可となり、`CatToken::decode` で token が拒否される。
  Composite の `or` クレームの分岐評価は未実装のため、未知キーを除いた部分受理は行わない
- テストは単体 8 件 (`tests/test_c4m/moqt.rs`) と PBT 2 件
  (`pbt/tests/prop_c4m/moqt.rs`) を追加した。単体テストは §2.1.2 の exact / prefix match の
  既知バイト列、決定論的エンコード、キー 0 の欠落・型不一致・空 scopes・重複・非整数キー・
  未知キー、i64 で表せない整数キー、エラー優先順、§2.1.2 の 3 番目の例の map 形式 match の
  拒否、claims set / token 経由の拒否、旧 array-only の拒否を固定する。PBT は claim の
  roundtrip と、キー 0 以外のキーを追加すると必ず拒否されることを検証する
- `SCOPE_VECTORS` の `moqt` claim 値は draft-02 §A.5 の公式値とバイト一致する map 形式へ
  更新した。label (327) と `moqt-reval` の表現は draft-01 のままとし、compact トークンと
  HMAC 署名は claim 値の変更に合わせて再計算した暫定値である

既知の制約: Composite を実装する際は claim set 単位の評価へ移す必要がある。moqt claim 以外の
C4M 部分 (label / `moqt-reval` / action 番号など) の draft-02 追随は後続の issue で行う。

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| 新しい map の既知バイト列で encode / decode / authorize が一致すること | `tests/test_c4m/moqt.rs` の `draft_02_exact_match_example_round_trips_and_authorizes` / `draft_02_prefix_match_example_round_trips_and_authorizes` / `claim_encoding_is_deterministic_map_keyed_by_zero` |
| キー 0 の欠落・型不一致・空 scopes と、未知キー付き claim を検証すること | 同ファイルの `claim_decode_errors_follow_fail_closed_rules` / `example_three_map_form_match_is_rejected_as_cddl_violation` |
| 未知の制限が加わった token を認可せず、旧 array-only の値を新形式として誤受理しないこと | 同ファイルの `claims_set_with_unknown_moqt_claim_key_is_not_acceptable` / `token_with_unknown_moqt_claim_key_is_rejected` / `claim_decode_errors_follow_fail_closed_rules`、`pbt/tests/prop_c4m/moqt.rs` の `unknown_claim_key_is_always_rejected` |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test -p shiguredo_moqt --test test_c4m` / `cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` /
`cargo test -p pbt` / `cargo test -p moq-sub` / `cargo test -p tokio-moq` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` はすべて成功した。

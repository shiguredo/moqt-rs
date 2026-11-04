# catreplay=1 を接続単位の再利用制限として扱う

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §3 (Replay Protection without DPoP) と §7.2 (Replay Protection for Bearer Tokens)](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m/cat.rs` は CLAIM_CAT_REPLAY を定義するが、`CatClaims` で型付きに解釈せず raw に保持する。
`CatClaims::validate` は時刻・issuer・audience を確認し、cti と接続の束縛は扱わない。
proof の jti を扱う `DpopReplayCache` は bearer token の接続束縛とは別物である。

## 設計方針

Sans I/O のまま、呼び出し側から渡す接続識別子と時刻で bearer token の接続束縛を検証できる API と状態を用意する。
`CatClaims` に `catreplay` の型付き表現を追加し、decode / encode と raw claim の重複判定を揃える。0 / 1 / 2 以外の値や整数でない値を拒否する。
catreplay=1 では cti / exp を必須にし、同じ接続上の SETUP と後続 action で再使用を許し、別接続で拒否する。
bearer 専用の `BearerReplayCache` は受信者が所有し、その受信者が受理する token 群と全接続で共有する。接続ごとに作り直さず、アプリケーションが発行する opaque な接続識別子を cti に束縛する。接続識別子は cache に記録が残る間は再利用しない。
同じ cache で許容 leeway を固定し、通常の期限検証と保持期限の両方に同じ値を使う。`exp + leeway` が有限であることを確認し、現行の `CatClaims::validate` と同じく参照時刻がこの期限と等しい間は受理可能とする。
切断で束縛を削除せず、参照時刻が保持期限を超えた後にだけ解放する。未失効の記録を追い出して別接続での再利用を許す処理は追加しない。
catreplay=0 / 省略は replay 保護なしで cache の記録を追加しない。2 は本実装では未対応として拒否し、0 と同じ扱いにはしない。
`CatToken` に bearer 用の公開検証入口を追加し、署名検証 → `CatClaims::validate` → 要求 action の scope 認可 → cache の接続照合・登録の順に実行する。全検証が成功したときだけ束縛状態を変更する。`cnf` 付き token はこの入口では拒否し、0418 の DPoP 検証へ渡す。
既存の `CatClaims::validate` は stateless な claims 検証として維持し、単独では接続束縛を保証しないことを API の説明で区別する。
ネットワークやリレー・クラスタの実装は追加しない。

## 完了条件

- 同一接続の複数 action は通り、同じ cti の別接続利用は切断後も受理可能期限まで拒否されること。
- cti / exp 欠落、失効時刻と leeway の境界、状態解放を検証すること。
- 署名・scope・通常の時刻検証を省略せず、検証失敗した token が束縛状態を汚染しないこと。
- 接続識別子と cache の所有者・保持期間を API の説明で明確にすること。
- catreplay=2 と不正な値を拒否し、0 / 省略では cache に記録しないこと。

## 依存・関連

- 関連: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)

## 差分の確認範囲

network I/O やリレー・クラスタ間の状態共有は実装しない。CWT 形式の DPoP proof の検証は
0418 で行う。docs / README / skills の利用者向け説明の更新は 0420 で行う。CHANGES.md は
CODEBASE の方針により更新しない。

## 解決方法

draft-02 §3 の `catreplay=1` を「単一接続での再利用」の意味で扱う Sans I/O の API を追加した。

- `src/c4m/cat.rs` に `CatReplay` (NotProtected=0 / SingleConnection=1 /
  ImplementationDefined=2) を追加し、`CatClaims::cat_replay` として型付きで保持する。
  decode は unsigned integer だけを受理し、負値・非整数は `UnexpectedType("catreplay")`、
  0 / 1 / 2 以外は `InvalidCatReplayValue(u64)` で拒否する。encode は整数で書き出し、
  `is_typed_claim_key` に追加したため型付きフィールドと raw の重複は
  `DuplicateClaim(308)` になる
- `BearerReplayCache` を追加した。受信者が所有し、受理するトークン群と全接続で共有する。
  `cti` → (接続識別子, 保持期限 = `exp` + leeway) を `BTreeMap` に保持し、コンストラクタで
  leeway を固定する。参照時刻が保持期限を超えた記録だけを解放し、未失効の記録は追い出さ
  ない。同じ `cti` が同じ接続なら再利用を許し、別の接続なら `ConnectionMismatch` で拒否
  する。空の `cti` / 接続識別子は `EmptyCti` / `EmptyConnectionId` で拒否する。参照時刻は
  単調増加を前提とし、`cti` は発行者をまたいで一意であることを前提とする (doc に明記)
- `CatToken::verify_bearer` を追加した。順序は `cnf` 付きの拒否 →
  `verify_with` による署名検証 → `CatClaims::validate` (leeway は cache と同じ値) →
  要求 action の scope 認可 → `catreplay` の判定である。cache の変更は最後の
  `check_and_bind` だけであり、それ以前の失敗では束縛状態を変更しない
- `catreplay` が 1 のときは `cti` と `exp` を必須とし (`CatReplayRequiresClaim`)、
  `exp` + leeway が有限でなければ拒否する。2 は `UnsupportedCatReplay` で拒否し、
  0 または省略なら replay 保護なしとして cache に記録しない。`cnf` 付きトークンは
  `BearerConfirmationNotSupported` で拒否する
- `CatClaims::validate` は stateless な検証のまま維持し、`catreplay` の必須条件も接続束縛
  も検証しないことを doc に明記した
- テスト / PBT: 同一接続の複数 action、別接続の拒否と保持期限後の解放、期限境界
  (`==` は受理、`>` は拒否)、`cti` / `exp` 欠落、失敗時の非汚染 (既存束縛がある状態を
  含む)、`catreplay` 2 / 0 / 省略、署名検証オプション、`exp` + leeway の非有限、
  decode / encode / raw 重複の各テストと、`catreplay` の roundtrip、decode の受理集合、
  cache の束縛・延長・解放の PBT を追加した

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| 同一接続の複数 action は通り、同じ cti の別接続利用は切断後も受理可能期限まで拒否されること | `tests/test_c4m/cat/bearer.rs` の `verify_bearer_allows_multiple_actions_on_same_connection` / `verify_bearer_rejects_other_connection_until_retention`、`bearer_replay_cache_rejects_other_connection_until_retention` |
| cti / exp 欠落、失効時刻と leeway の境界、状態解放を検証すること | 同 `verify_bearer_requires_cti_and_expiration` / `verify_bearer_expiration_boundary_matches_cache_leeway` / `verify_bearer_rejects_non_finite_retention`。状態解放は 1 行目のテスト、境界と解放の cache 側は `bearer_replay_cache_*` で検証する |
| 署名・scope・通常の時刻検証を省略せず、検証失敗した token が束縛状態を汚染しないこと | 同 `verify_bearer_failures_do_not_pollute_cache` / `verify_bearer_uses_signature_verification_options` |
| 接続識別子と cache の所有者・保持期間を API の説明で明確にすること | `BearerReplayCache` / `BearerVerificationOptions` / `CatToken::verify_bearer` の rustdoc と `CatClaims::validate` の doc |
| catreplay=2 と不正な値を拒否し、0 / 省略では cache に記録しないこと | 同 `verify_bearer_rejects_implementation_defined_replay` / `verify_bearer_without_replay_protection_does_not_bind`、`tests/test_c4m/cat.rs` の `invalid_cat_replay_values_are_rejected`、PBT の `cat_replay_decode_*` / `bearer_replay_cache_matches_model` |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test -p shiguredo_moqt` / `cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` /
`cargo test -p pbt` / `cargo test -p moq-sub` / `cargo test -p tokio-moq --lib with_auth_parameters` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` はすべて成功した。

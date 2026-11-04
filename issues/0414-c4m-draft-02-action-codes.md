# C4M の SETUP 統合と SUBSCRIBE_TRACKS action に対応する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02 §2.1 Table 1、§4.2](../refs/moq/draft-ietf-moq-c4m-02.txt) の変更に追従する。

## 現状

`src/c4m.rs` の `MoqtAction` は ClientSetup=0 / ServerSetup=1 を別 action とし、ALL は 9 要素で、SUBSCRIBE_TRACKS=9 がない。
`authorization_context` は旧 DPoP の文字列 action も返す。

## 設計方針

draft 02 の SETUP=1 と SUBSCRIBE_TRACKS=9 に合わせ、enum、数値の変換、表示と scope の評価を更新する。
`ClientSetup` / `ServerSetup` を `Setup` に統合し、`SubscribeTracks` を追加する。`ALL` は 1〜9 の 9 要素とし、`key` / `from_key` / `name` と利用箇所を揃える。
旧 ClientSetup=0 は未知の整数として scope 内に保持できるが、`from_key(0)` は `None` とし、既知 action を認可する alias にしない。他の未知整数も同じ扱いとする。
旧 JWT 用の文字列 `authorization_context` は互換 API として残し、戻り値を `Option<&'static str>` にする。`Setup` は `Some("SETUP")`、旧形式に定義のない `SubscribeTracks` は `None` とする。`matches_authorization_context` と旧 JWT 側の呼び出し箇所は未対応 action を明示的に拒否し、独自の文字列 action を追加しない。
CWT の数値 actx は同じ action 定義を使うが、その codec への組み込みと照合は 0416 / 0418 で行う。本 issue の完了に CWT proof の先行実装を要求しない。
SUBSCRIBE_TRACKS の認可表現を追加しても、namespace 機能の実装は行わない。

## 完了条件

- SETUP=1 と SUBSCRIBE_TRACKS=9 の既知バイト列・認可を検証すること。
- 未知 action と旧 0 を既知 action の許可として扱わず、要求 action を含まない scope は拒否すること。
- 旧 JWT 用の文字列 API が `Setup` を扱い、`SubscribeTracks` を未対応として拒否すること。CWT proof と scope の数値 action の一致は 0416 / 0418 で検証すること。

## 依存・関連

- 関連: [0416](0416-c4m-draft-02-cwt-dpop-proof-codec.md)
- 関連: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)
- 関連: [0419](0419-c4m-draft-02-test-vectors.md)
- 関連: [0420](0420-c4m-draft-02-documentation-and-coverage.md)

## 差分の確認範囲

- `MoqtAction` を draft-02 §2.1 Table 1 の SETUP=1 / SUBSCRIBE_TRACKS=9 に更新し、
  `ALL` / `key` / `from_key` / `name` を 1〜9 に揃え、旧 CLIENT_SETUP=0 を未知の整数として
  扱う
- 旧 JWT 形式の `authorization_context` を `Option<&'static str>` にし、DPoP の
  `verify_action` / `verify_authorization_context` を `None` の action で拒否する
- テスト / PBT / admin wildcard ベクタの扱いの更新

CWT proof の actx への組み込みと数値 action の照合は後続 (0416 / 0418)。docs / README /
skills の C4M draft-02 記述の更新は 0420 で行う。`docs/c4m.md` の action 表と
`authorization_context` のサンプルは、この変更後は一時的にコードと不一致になる。
action 0 の公式ベクタの期待値の復元は 0419 で行う。CHANGES.md は CODEBASE の方針により
更新しない。

## 解決方法

`MoqtAction` を draft-02 §2.1 Table 1 の action コードへ更新し、旧 JWT 形式の文字列 API を
`Option` 化した。

- `Setup=1` に統合し、`ClientSetup` / `ServerSetup` を削除した。`PublishNamespace` 〜
  `TrackStatus` は 2〜8 のまま、`SubscribeTracks=9` を追加した。`ALL` は key 1〜9 の
  9 要素である
- `from_key(0)` は `None` を返し、旧 CLIENT_SETUP=0 を既知 action の alias にしない。
  未知の整数も `None` とし、i64 で表せる範囲の未知の整数は scope の `actions` に
  整数のまま保持する (i64 で表せない整数は claim 全体を拒否する)。認可は
  `MoqtAction::key` との照合だけなので、未知の整数が既知 action の許可になることはない
- `authorization_context` は `Option<&'static str>` を返す。`Setup` は `Some("SETUP")`、
  7 つの既存 action は従来の文字列、旧 JWT 形式に定義のない `SubscribeTracks` は `None`。
  `matches_authorization_context` と DPoP の `verify_action` は `None` を不一致として
  拒否する。独自の文字列 action は追加していない
- `moqt_admin_wildcard` ベクタは draft-02 §A.5 の payload (actions 0〜8) を変更せず、
  action 0 の認可期待値だけを既知 action の検証から分離した。§A.5 の admin ベクタが
  §2.1 Table 1 と不整合であることをテストと doc に明記した
- テストを追加した: SETUP / SUBSCRIBE_TRACKS の key と既知バイト列 (a10081818101 /
  a10081818109) の decode / encode / authorize、要求 action を含まない scope の拒否、
  未知の action 整数の保持と非認可、旧 JWT 文字列の厳密な対応表と DPoP の Setup / None の
  扱い。PBT に未知 action の保持と非認可、既知アクションの組み合わせの生成を追加した

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| SETUP=1 と SUBSCRIBE_TRACKS=9 の既知バイト列・認可を検証すること | `tests/test_c4m/moqt.rs` の `setup_and_subscribe_tracks_known_bytes_round_trip` |
| 未知 action と旧 0 を既知 action の許可として扱わず、要求 action を含まない scope は拒否すること | 同 `unknown_action_*` / `scope_without_requested_action_is_rejected` / `action_integer_out_of_i64_range_is_rejected`、PBT の `unknown_action_keys_are_kept_and_never_authorize` |
| 旧 JWT 用の文字列 API が `Setup` を扱い、`SubscribeTracks` を未対応として拒否すること (CWT proof と scope の数値 action の一致は 0416 / 0418) | 同 `action_mapping` / `legacy_authorization_context_table`、`tests/test_c4m/dpop.rs` の `authorization_context_errors`、`pbt` の `authorization_context_matches_model` |

検証コマンド `cargo fmt --all -- --check` / `cargo clippy --workspace --all-targets -- -D warnings` /
`cargo test -p shiguredo_moqt` / `cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` /
`cargo test -p pbt --test prop_c4m` / `cargo test -p moq-sub` / `cargo test -p tokio-moq --lib with_auth_parameters` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` はすべて成功した。

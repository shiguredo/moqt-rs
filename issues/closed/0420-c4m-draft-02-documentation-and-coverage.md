# C4M draft 02 の仕様参照と認可 API の説明を更新する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

[draft-ietf-moq-c4m-02](../../refs/moq/draft-ietf-moq-c4m-02.txt) の全文差分に沿って、
公開する認可 API と利用上の責務を説明する。

## 現状

src/c4m.rs と各サブモジュール、docs/c4m.md、skills/shiguredo-moqt/SKILL.md は draft 01 の
JWT proof と moqt scopes 配列、旧ベクタを前提にする。
crates/tokio-moq/src/lib.rs の c4m_authorization_token は token をそのまま運ぶが、
token の運搬と認可・proof 検証を区別した説明が必要である。
README.md の対応仕様、Cargo.toml の C4M 依存コメント、docs/missing-features.md の基準版・CWT proof 未実装表・節番号にも draft 01 の記述が残る。

## 設計方針

実装の追従に合わせ、署名検証だけ・claim validation・scope 認可・DPoP / replay の検証を区別する。
「token を decode できる」ことを「action を認可できる」こととして説明しない。
I/O、時計、接続識別子、replay state の所有者を明記する。
暫定 claim label と未確定の proof の運搬方法を正式仕様として確定させない。
未確定の actx / ath 番号や AUTHORIZATION_TOKEN 内の独自 proof envelope を黙って割り当てない。

## 変更対象

- `README.md` の C4M の対応仕様・使用例
- `docs/c4m.md` と `docs/missing-features.md` の C4M の対応版・実装状況・未対応項目
- `skills/shiguredo-moqt/SKILL.md` の C4M の対応版・API・使用例・仕様参照
- `src/c4m.rs` と各サブモジュールの公開 API・モジュール説明、`Cargo.toml` の C4M 依存コメント
- `crates/tokio-moq/src/lib.rs` の `c4m_authorization_token` の token 運搬に関する説明

0404 が扱う MOQT の説明更新とは C4M の範囲で分担し、未対応項目は先行 issue の実装結果を確認して更新する。
0412〜0419 が完了しても、0345 / 0346 / 0356 の既知ギャップまで解消済みとは扱わない。各課題の実装状況を確認し、残る制約と利用側の追加検証を説明する。0345 が未完了なら、サンプルでは非ゼロの moqt-reval を無条件に認可せず、利用側での拒否または再検証能力に応じた判定・実行を示す。

## draft 01 → 02 の差分対照

| 変更 | 追跡先・判定 |
| --- | --- |
| moqt claim value の map 化、key 0 の scopes | 0412 |
| 未知 map key がある claim set の fail-closed | 0412 |
| 付録の Private Use claim label -65537 / -65538 | 0413 |
| CLIENT_SETUP / SERVER_SETUP の SETUP=1 への統合、SUBSCRIBE_TRACKS=9 | 0414 |
| catreplay=1 の接続束縛、cti / exp 必須、切断後も期限まで保持 | 0415 |
| catreplay=0 / 省略 / 2 の違い、複数検証者間で state が必要という説明 | 0415 と本 issue。分散状態の実装は対象外 |
| JWT から COSE_Sign1 の CWT proof へ変更 | 0416 |
| protected alg / typ / kccs、cti / iat / actx / ath の CBOR 表現 | 0416 |
| COSE_Key の ckt=5 束縛と jkt の互換性 | 0417 |
| proof なしの cnf 付き token の禁止、raw token bytes の ath | 0418 |
| action ごとの bytes の完全一致、cti replay・鮮度 | 0418 |
| Appendix A の CBOR / COSE ベクタ、alg / MAC / 署名・異常系の更新 | 0419 |
| token 取得側の DPoP は RFC 9449 の方式を継続 | 本 issue。認可サーバーの実装は追加しない |
| 無関係の deployment 間の鍵共有を避ける、proof と token の message size を考慮 | 本 issue。0379 のサイズ上限の説明と整合させる |
| generic DPoP draft への依存削除、節移動、参照・IANA の更新 | 本 issue。汎用 JWT 機能を残す場合の位置付けも説明する |
| moqt-reval の基本的な再検証規則 | 本文の意味は継続。0345 の既存課題を重複起票しない |

## 完了条件

- README.md、docs/c4m.md、docs/missing-features.md、公開 SKILL、モジュール説明とサンプル、Cargo.toml の C4M 依存コメントが実装済みの draft 02 API に一致すること。
- `c4m_authorization_token` が token bytes を運ぶだけで認可・proof 検証を行わないことを、利用側が判断できる説明にすること。
- 必須の検証と呼び出し側に残る責務、暫定番号・未対応項目を利用者が判断できること。
- 旧 JWT / compact / key 3 の互換 API を残す場合、現在の C4M の標準形式と混同しないこと。
- 0345 / 0346 / 0356 と新しい追従 issue の関係が明確であること。
- 既知ギャップが未完了の場合も対応済みと断定せず、特に非ゼロ moqt-reval を既定の `CatClaims::validate` だけで受理してよいと説明しないこと。

## 依存・関連

- 先行: [0412](0412-c4m-draft-02-moqt-claim-map.md)
- 先行: [0413](0413-c4m-draft-02-provisional-claim-labels.md)
- 先行: [0414](0414-c4m-draft-02-action-codes.md)
- 先行: [0415](0415-c4m-draft-02-connection-scoped-bearer-replay.md)
- 先行: [0416](0416-c4m-draft-02-cwt-dpop-proof-codec.md)
- 先行: [0417](0417-c4m-draft-02-cose-key-thumbprint-binding.md)
- 先行: [0418](0418-c4m-draft-02-cwt-dpop-validation.md)
- 先行: [0419](0419-c4m-draft-02-test-vectors.md)
- 関連: [0345](../0345-bug-c4m-moqt-reval.md)
- 関連: [0346](../0346-bug-c4m-dpop-validation.md)
- 関連: [0356](../0356-test-c4m-pbt-model-fuzz-gaps.md)

## 差分の確認範囲

- `README.md` / `crates/README.md` / `crates/tokio-moq/README.md` の C4M の対応仕様・使用例
- `docs/c4m.md` (draft-02 への全面更新、検証 API の責務、残る制約)、
  `docs/missing-features.md`、`docs/moqt.md`
- `skills/shiguredo-moqt/SKILL.md` の C4M の対応版・API・使用例・注意事項
- `src/lib.rs`、`src/c4m.rs`、`src/c4m/{cat,cbor,cose,base64url,dpop,jwk}.rs` の
  モジュール説明 / 公開 API の説明 (実装挙動は変更しない)
- `crates/tokio-moq/src/lib.rs` / `src/moqt_client.rs` の token 運搬と検証責務の説明
- `Cargo.toml` の C4M 依存コメント

実装コードとテストは変更しない。CHANGES.md は CODEBASE の方針により更新しない。
`docs/missing-features.md` の既存行に残る他機能の issue パスは、今回の追加分ではないため
変更していない。

## 解決方法

draft-02 の実装に合わせて、公開する認可 API と利用上の責務の説明を更新した。

- `docs/c4m.md` に「検証 API の責務」を新設し、decode / verify / validate / authorize /
  `verify_bearer` / `CwtDpopProof::verify_against_cat_token` / 旧 JWT 一括検証の
  「検証するもの・しないもの」を表で整理した。decode できることと認可できることを
  区別し、時計・I/O・接続識別子・replay state・`moqt-reval` の再検証は利用側の責務と
  明記した
- 現行仕様の記述を draft-02 に更新した (moqt claim の map 形式と未知キーの fail-closed、
  Private Use の暫定 claim label、action 1 〜 9、`catreplay` 0 / 1 / 2、CWT 形式 proof の
  alg / typ / kccs と cti / iat / actx / ath、ckt / jkt、ath の提示バイト列、鮮度、
  cti replay、proof なしの cnf 付きトークンの禁止)。旧 draft-01 の compact 形式・
  alg=-4・key 3・旧 JWT 形式は互換 API として区別した
- 残る制約を明記した: Composite の評価未実装 (raw 保持のみ、top-level の moqt と
  併存する場合は fail-closed に扱うこと)、CWT proof の運搬方法が未確定で独自 envelope を
  割り当てないこと、`catreplay` 2 未対応、非ゼロ `moqt-reval` は既定検証だけでは受理しない
  こと、cnf と catreplay の併存拒否、旧 JWT の replay cache と key 3 の既知の不具合、
  RSA 未対応、付録 A.5 の action 0 と A.6 の暫定 Composite の不整合
- `README.md` の C4M サンプルを bearer 専用の fail-closed な例に直し、`cnf` 付きトークンと
  非ゼロ `moqt-reval` を拒否してから `verify_bearer` に接続識別子と replay cache を渡す
  形にした。`cnf` 付きトークンは `CwtDpopProof::verify_against_cat_token` を使うことを
  明記した
- `skills/shiguredo-moqt/SKILL.md` の C4M の対応版・API 一覧・使用例・注意事項を
  draft-02 に更新し、actx / ath の label が TBD であること、呼び出し側が同じ設定を
  発行・デコード・検証で使うことを明記した
- `crates/tokio-moq/src/lib.rs` の `c4m_authorization_token` と `build_setup_options` の
  説明を、token のバイト列を運ぶだけでデコード・署名検証・クレーム / scope 認可・
  DPoP 検証を行わないこと、検証は受理側の責務であることが分かる形に分離した
- `Cargo.toml` と各モジュール doc の C4M の対応版・依存コメントを draft-02 に更新した

## 完了条件の検証

| 完了条件 | 検証 |
| --- | --- |
| README.md、docs/c4m.md、docs/missing-features.md、公開 SKILL、モジュール説明とサンプル、Cargo.toml の C4M 依存コメントが実装済みの draft 02 API に一致すること | `README.md` / `docs/c4m.md` / `skills/shiguredo-moqt/SKILL.md` の API 名・シグネチャを src と突き合わせ、モジュール doc を rustdoc `-D warnings` で検証 |
| `c4m_authorization_token` が token bytes を運ぶだけで認可・proof 検証を行わないことを、利用側が判断できる説明にすること | `crates/tokio-moq/src/lib.rs` の `c4m_authorization_token` / `build_setup_options` と `crates/tokio-moq/README.md` の説明 |
| 必須の検証と呼び出し側に残る責務、暫定番号・未対応項目を利用者が判断できること | `docs/c4m.md` の「検証 API の責務」「仕様側の未確定事項」「残る制約」、`docs/missing-features.md`、SKILL の「残る制約」「注意事項」 |
| 旧 JWT / compact / key 3 の互換 API を残す場合、現在の C4M の標準形式と混同しないこと | `docs/c4m.md` と SKILL で「互換 API」「現在の標準形式は COSE / CWT」と明記 |
| 0345 / 0346 / 0356 と新しい追従 issue の関係が明確であること | `docs/missing-features.md` の `moqt-reval` 再検証 (0345)、旧 JWT replay cache / key 3 (0346)、PBT モデル / fuzz の穴 (0356) の行に既存 issue を記載 |
| 既知ギャップが未完了の場合も対応済みと断定せず、特に非ゼロ moqt-reval を既定の `CatClaims::validate` だけで受理してよいと説明しないこと | `docs/c4m.md` の残る制約と SKILL の注意事項で、利用側の再検証または拒否が必要と明記 |

検証コマンドは、`cargo fmt --all -- --check` / `cargo clippy -p shiguredo_moqt --all-targets -- -D warnings` /
`cargo clippy -p pbt --all-targets -- -D warnings` / `cargo test -p shiguredo_moqt` /
`cargo test -p shiguredo_moqt --features aws-lc-rs --test test_c4m` / `cargo test -p pbt` /
`RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p shiguredo_moqt` /
`prek run --files <変更ファイル>` を実行して成功した。`cargo clippy --workspace --all-targets` は、
この環境では `shiguredo_audio_device` のビルドに libpulse が必要なため、libpulse を用意した
環境で実行した。CI を含め libpulse の無い環境ではワークスペース一括の clippy は実行できない。

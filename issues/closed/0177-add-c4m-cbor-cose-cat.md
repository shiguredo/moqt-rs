# C4M の CBOR / COSE / CAT コーデックと認可を追加する

- Created: 2026-09-27
- Completed: 2026-09-27
- Branch: feature/add-c4m-cbor-cose-cat
- Polished: {YYYY-MM-DD}

## 目的

MOQT の認可トークン (C4M: draft-ietf-moq-c4m-01) を扱う `src/c4m/` を追加する。認可サーバーがトークンを発行し、クライアント / サーバーがそれを検証して MOQT アクションの可否を判定できるようにする。

- CBOR (RFC 8949) のコーデックを自前で持つ。MSF が nojson を自前で使っているのと同じ方針で、CWT / COSE が必要とする範囲 (決定論的エンコードを含む) を no_std で実装する
- COSE (RFC 9052 / RFC 9053) の構造と、CAT (CTA-5007-B) のトークン / クレームを実装する
- 暗号処理 (HMAC / ECDSA / EdDSA) は trait に切り出し、aws-lc-rs 実装を optional feature にする。既定ビルドは no_std のままで暗号実装を一切リンクしない
- トークンの発行 (署名) と検証の両方を提供する。認可サーバー用途 (moqt-py など) では発行が必須になる
- DPoP はトークン側の `cnf` (JWK サムプリント) と `catdpop` に加え、JWT (JWS compact) の DPoP proof の検証と発行までを扱う。CWT 形式の DPoP proof (`dpop-proof+cwt`) は actx の claim label が TBD のため対象外とする

## 現状

- `src/` に CBOR / COSE / CAT / C4M の実装は無い。`src/message_parameter.rs` の `AUTHORIZATION_TOKEN` は token type / value のバイト列を運ぶだけで、type 0x01 (CAT) の解釈はしていない
- draft-ietf-moq-c4m-01 §7.1.1 は Auth Token Type 0x01 の Token Payload を「CBOR でエンコードした CWT としての CAT」と定義し、§2 で `moqt` (claim key 327) / `moqt-reval` (claim key 328) を定義する。付録 A に CBOR エンコード・トークン構造・DPoP バインディング・認可マッチング・検証のテストベクタがある
- 参照仕様のうち RFC 8949 / RFC 9052 / RFC 9053 / RFC 8392 / RFC 8747 / RFC 4648 / RFC 7638 / RFC 8032 / RFC 9449 / RFC 7515 / RFC 7517 / RFC 9596 を `refs/cbor/` に追加した。CTA-5007-B (CAT 本体) は有償仕様で自由に取得できないため refs には置けない。claim key の値は IANA の CWT Claims レジストリと CWT Confirmation Methods レジストリで確認する
- 仕様側の未解決事項が 3 つある。実装では以下として扱い、コメントに残す
  - トークン直列化: §7.1.1 は CBOR の COSE_Sign1 / COSE_Mac0 (CWT) と読めるが、付録 A のベクタは `base64url(protected).base64url(claims).base64url(signature)` の 3 分割形式で、署名対象は ASCII の `protected.claims` である (HMAC-SHA256 で実測確認済み)。本実装は両形式を受理し、発行は形式を選べるようにする
  - HMAC-SHA256 の COSE アルゴリズム ID: 付録 A のベクタは -4 を使うが、IANA の COSE Algorithms レジストリで -4 は A192KW であり、HMAC 256/256 は 5 である (CTA-5007-B の実装である Akamai / Fastly の実装も 5 を使う)。検証は -4 と 5 の両方を受けて HMAC-SHA256 として扱い、compact / COSE のどちらの発行も RFC 9053 に合わせて 5 を書く
  - `cnf` の JWK サムプリント: §3.1.1 と付録 A のベクタは confirmation key 3 を使うが、IANA の CWT Confirmation Methods レジストリで 3 は kid であり、CTA 登録の jkt は 323 である。検証は 323 と 3 の両方を受ける。発行は 323 を既定とし、ドラフト準拠が必要な場合は 3 を選べるようにする

## 設計方針

### モジュール構成

| モジュール | 内容 |
| --- | --- |
| `c4m` | C4M 本体。`MoqtAction` / `MoqtScope` / `MoqtClaim` / `moqt-reval` / `cnf` / `catdpop` と認可判定 |
| `c4m::cbor` | RFC 8949 の CBOR コーデック。`Value` と決定論的 encode / 制限付き decode |
| `c4m::cose` | RFC 9052 の COSE 構造。protected / unprotected ヘッダ、COSE_Sign1 (tag 18) / COSE_Mac0 (tag 17)、アルゴリズム定義 |
| `c4m::crypto` | `CoseCrypto` trait、`CoseKey`、エラー型。aws-lc-rs 実装は feature 分岐 |
| `c4m::cat` | CAT のクレーム (`CatClaims`) とトークン (`CatToken`) および発行ビルダー (`CatTokenBuilder`) |
| `c4m::jwk` | JWK (RFC 7517) と JWK サムプリント (RFC 7638) |
| `c4m::jwt` | JWS compact (RFC 7515) の JWT |
| `c4m::dpop` | DPoP proof の検証と発行 (draft-nandakumar-moq-generic-dpop-proof) |

- `mod.rs` は使わず `src/c4m.rs` + `src/c4m/*.rs` の構成にする
- re-export はしない。利用側は `shiguredo_moqt::c4m::cat::CatToken` のように参照する
- 外部 RNG や時計は受け取らない。Sans I/O を維持する (`reference_time` は引数で渡す)

### CBOR

- `Value` は `Unsigned` / `Negative` / `ByteString` / `TextString` / `Array` / `Map` / `Tag` / `Bool` / `Null` / `Undefined` / `Float` / `Simple` を持つ
- encode は RFC 8949 §4.2 の決定論的エンコードにする (最短長の整数と長さ、マップはキーのエンコード済みバイト列順、float は値を保つ最短幅 f16 / f32 / f64、NaN は `f9 7e 00`)
- decode は definite / indefinite の両方を受ける。マップの重複キー、UTF-8 でないテキスト、ネスト深度の上限超過はエラーにする
- 事前確保 (`Vec::with_capacity`) はしない。追加の依存は入れない (f16 変換も自前で実装する)

### COSE / crypto

- protected / unprotected ヘッダ、`alg` / `kid` / `typ` / `content type` を扱う。未知のヘッダは保持して決定論的 encode で再現する
- `Sig_structure` (`"Signature1"`) と `MAC_structure` (`"MAC0"`) を組み立てる。CWT タグ 61 と COSE タグ 17 / 18 は decode で許容し、encode では付与する
- `CoseCrypto` trait は `sign` / `verify` を持ち、`CoseKey` (Symmetric / Ec2 / Okp) と `Algorithm` (HMAC 256/384/512、ES256/384/512、EdDSA) を引数にする。trait は no_std のコアに置き、実装は `#[cfg(feature = "aws-lc-rs")]` に閉じる
- `Cargo.toml` に `aws-lc-rs = { version = "1.18", optional = true }` と `base64ct = { version = "1.8", default-features = false, features = ["alloc"] }` を追加し、`aws-lc-rs` feature は `dep:aws-lc-rs` にする。既定ビルドと thumbv7em-none-eabihf ビルドは現状のまま通す
- ECDSA の署名は COSE の固定長 r||s (ES256 は 64 バイト) を使う。EdDSA は Ed25519 のみとする。RSA (PS256 など) は対象外とし、未知のアルゴリズムは `UnsupportedAlgorithm` を返す

### CAT

- `CatClaims` は `iss` / `sub` / `aud` / `exp` / `nbf` / `iat` / `cti` を型付きで持ち、`exp` / `nbf` / `iat` は整数と浮動小数の両方を受ける。`cti` はバイト列とテキストの両方を受ける (付録 A のベクタはテキスト)
- CAT 固有クレームは `catv` / `catu` / `catnip` / `catalpn` / `cath` / `catgeocoord` / `catgeoalt` / `catdpop` などの claim key を定数として公開し、値は未解釈の `cbor::Value` として保持する。IANA 登録と付録 A のベクタで値型が一致しない claim (catv / catu など) があるため、型付きにするのは C4M が意味論を定義する `moqt` / `moqt-reval` / `cnf` / `catdpop` に限る
- `CatToken` は compact 形式と COSE 形式を自動判別して decode する。入力はバイト列 (CBOR / compact ASCII) と base64url テキストの両方を受ける
- 検証は `CoseCrypto` 実装を引数に取る。`exp` / `nbf` / `iss` / `aud` の検証は `reference_time` と期待値リストを渡す純関数にする。付録 A.6 の期待エラー (Expired / NotYetValid / InvalidIssuer / InvalidAudience / SignatureVerificationFailed 相当) に対応する
- 発行は `CatTokenBuilder` でクレームと `moqt` スコープを組み立て、`build_compact` / `build_cose` で署名する

### C4M 認可

- `MoqtAction` は 0〜8 (CLIENT_SETUP / SERVER_SETUP / PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE / REQUEST_UPDATE / PUBLISH / FETCH / TRACK_STATUS) を持つ。未知の値も保持できるようにする
- `MoqtScope` はアクション配列、名前空間マッチ配列 (exact bstr / `[1, prefix]` / `[2, suffix]` / nil)、トラックマッチ (exact / prefix / suffix) を持つ
- マッチングは §2.1 の規則に従う。nil は名前空間の末尾にのみ現れ、末尾以外の nil は不正とする。nil が末尾に無い場合は前方一致で長い名前空間も許す
- `CatClaims::authorize(action, namespace, track_name)` で可否を判定する

### テスト

- `tests/test_c4m.rs` + `tests/test_c4m/` に CBOR / COSE / CAT / C4M のテストを置き、付録 A のテストベクタを固定する
- `pbt/tests/prop_c4m/` に CBOR のラウンドトリップと認可マッチングの PBT を置く。PBT は noprop を使う
- `fuzz/fuzz_targets/` に CBOR decode / compact token decode / COSE token decode / DPoP proof decode / DPoP actx 検証のターゲットを追加する
- aws-lc-rs feature が必要なテストは `#[cfg(feature = "aws-lc-rs")]` で分岐し、CI の test ジョブに `cargo test -p shiguredo_moqt --features aws-lc-rs` を追加する
- `docs/IMPLEMENTATION.md` のモジュール表と未対応節を更新する
- `CHANGES.md` に追加を記載する

## 完了条件

- `src/c4m/` の公開 API で CAT トークンの発行 (HMAC-SHA256 / ES256 以上) と検証ができること
- 付録 A.2〜A.6 のテストベクタ (CBOR エンコード、トークン構造、DPoP バインディング、認可マッチング、検証結果) をテストで固定できること
- `moqt` クレームの exact / prefix / suffix / nil / 複数スコープのマッチングが §2.1 の例とテストベクタに一致すること
- 既定ビルドが no_std (`cargo build -p shiguredo_moqt --target thumbv7em-none-eabihf`) で通ること
- `cargo test --workspace` と `cargo test -p shiguredo_moqt --features aws-lc-rs` が通ること
- `make fmt` / `make clippy` が通ること
- 仕様側の未解決事項 (-4 / jkt の claim key / 直列化) の扱いがコードコメントに残っていること

## 設計上の制約

- CTA-5007-B 本体は有償仕様のため参照できない。IANA レジストリ・C4M ドラフト・公開実装 (Akamai / Fastly) で確認できた範囲を実装し、確認できないクレームの意味論 (catu のマッチ評価など) は実装しない
- CWT 形式の DPoP proof (`dpop-proof+cwt`) の検証は actx の claim label が TBD のため扱わない。JWT 形式 (C4M §3.1.2 が参照する形式) は検証と発行を実装する

## 解決方法

`src/c4m/` に CBOR / COSE / CAT / C4M の実装を追加し、`CoseCrypto` trait と aws-lc-rs feature による署名・検証、トークンと DPoP proof の発行・検証、`moqt` クレームの認可判定を実装した。

- `src/c4m/cbor.rs`: CBOR (RFC 8949)。決定論的エンコード、f16/f32/f64 の最短幅、definite / indefinite のデコード、深度制限、重複キー (NaN / -0.0 の等価を含む) の拒否
- `src/c4m/cose.rs`: COSE_Sign1 / COSE_Mac0 / CWT タグ、protected / unprotected のバケット規則 (crit / alg / typ / 重複ラベル) と Sig_structure / MAC_structure
- `src/c4m/crypto.rs` + `src/c4m/crypto/aws_lc_rs.rs`: `CoseCrypto` trait (署名 / 検証 / ハッシュ) と aws-lc-rs 実装 (HMAC 256/384/512、ES256/384/512、EdDSA)
- `src/c4m/cat.rs` + `src/c4m/jwk.rs` + `src/c4m/jwt.rs` + `src/c4m/dpop.rs` + `src/c4m/json.rs`: CAT のクレームとトークンの発行 / 検証、JWK とサムプリント、JWS compact、DPoP proof の検証と発行 (`ath` の必須化、鍵バインディング、actx、鮮度、jti リプレイ保護)
- `src/c4m.rs`: `MoqtAction` / `MoqtScope` / `MoqtClaim` の exact / prefix / suffix / nil マッチングと `catdpop`
- `src/name.rs`: DPoP の `tns` / `tn` 用に namespace / track name 単体のシリアライズとパースを追加
- テスト: 付録 A.2〜A.6 の全ベクタ (decode / 再エンコード / 署名検証 / 認可 / 検証結果)、発行と検証のラウンドトリップ (全対応アルゴリズム)、エラー経路、`pbt/tests/prop_c4m/` のラウンドトリップと認可のモデル検査、fuzz ターゲット 5 本
- ドキュメント: `docs/IMPLEMENTATION.md`、`README.md`、`skills/shiguredo-moqt/SKILL.md`、`CHANGES.md`、CI (aws-lc-rs feature の clippy / test / rustdoc)、`refs/cbor/` の一次資料

仕様側の未解決事項はコードコメントに残した (compact 形式の直列化、HMAC-SHA256 のアルゴリズム ID、`cnf` の jkt の claim key)。`/review-diff-code` を 3 周実行し、致命的・重要の指摘を全て修正した。

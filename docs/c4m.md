# C4M (Common Access Token for MoQ)

MOQT の認可トークンを扱う `c4m` モジュールの解説です。
[draft-ietf-moq-c4m-02](https://datatracker.ietf.org/doc/html/draft-ietf-moq-c4m-02) が定義するトークンと、その基盤となる CBOR、COSE、CWT、CAT を実装しています。
モジュール構成とアーキテクチャは [`moqt.md`](moqt.md)、他の仕様の実装状況は [`msf.md`](msf.md) / [`loc.md`](loc.md)、利用者向けの API 一覧は [`../skills/shiguredo-moqt/SKILL.md`](../skills/shiguredo-moqt/SKILL.md) を参照してください。

## 概要

- トークンは CAT (Common Access Token) です。CAT は CWT (CBOR Web Token) と COSE を使って署名または MAC されます
- draft-02 の標準的な直列化は COSE 形式 (COSE_Sign1 / COSE_Mac0) です。付録 A.3 以降のトークンのベクタは COSE 構造の base64url 表現です。旧 draft-01 の compact 形式は互換 API として残しています
- MOQT へは Auth Token Type `0x01` (CAT) の Token Value として載せます (§8.2.1)
- `moqt` クレーム (暫定 claim label -65537) でアクション、名前空間、トラック名の範囲を指定します。claim value は CBOR map で、キー 0 にスコープの配列を持ちます (§2.1)
- `moqt` クレームのキー 0 以外は将来の改訂のために予約されており、未知のキーを含む claim は fail-closed で拒否します (§2.1.1)
- アクションは SETUP=1 から SUBSCRIBE_TRACKS=9 までの 9 種類です (§2.1 Table 1)。旧 draft-01 の CLIENT_SETUP=0 は既知のアクションとして扱いません
- `catreplay` は 0 / 省略なら保護なし、1 なら接続単位の再利用制限、2 は実装定義です (§3)
- DPoP を使うとトークンをクライアントの鍵に束縛できます (`cnf` の `ckt` / `jkt` と `catdpop` の設定)。draft-02 の proof は CWT 形式 (§4.2) です。旧 JWT 形式の proof API も互換のために残しています
- Sans-I/O です。時計、接続識別子、replay cache、再検証の実行は利用側が持ちます。検証 API は引数で渡された時刻だけを使います
- 既定ビルドは `no_std` です。暗号処理は `CoseCrypto` trait に分離し、aws-lc-rs を使う実装を optional feature `aws-lc-rs` で提供します

## モジュール構成

| モジュール | 役割 |
| --- | --- |
| `c4m` | `MoqtAction`、`MoqtScope`、`MoqtClaim`、`Match`、`NamespaceMatch`、`CatDpop` |
| `c4m::cbor` | CBOR (RFC 8949) の encode / decode |
| `c4m::cose` | COSE (RFC 9052) の構造、ヘッダ、アルゴリズム |
| `c4m::crypto` | `CoseCrypto` trait、`CoseKey`、エラー型 (aws-lc-rs 実装は feature 分岐) |
| `c4m::cat` | CAT のクレーム、トークン、発行ビルダー、bearer 検証、接続束縛 |
| `c4m::jwk` | JWK (RFC 7517) と JWK サムプリント (RFC 7638) |
| `c4m::jwt` | JWS compact (RFC 7515) の JWT (旧 JWT 形式の DPoP proof 用) |
| `c4m::dpop` | 旧 JWT 形式の DPoP proof と、draft-02 §4.2 の CWT 形式 proof の codec と検証 |

re-export はしていないため、型は定義元から import します。

```rust
use shiguredo_moqt::c4m::cat::CatToken;
use shiguredo_moqt::c4m::crypto::CoseKey;
use shiguredo_moqt::c4m::{MoqtAction, MoqtClaim, MoqtScope};
```

## 実装状況

- トークン
  - CBOR (RFC 8949) のコーデック (決定論的エンコード、definite / indefinite のデコード、深度制限)
  - COSE (RFC 9052) の COSE_Sign1 (タグ 18) / COSE_Mac0 (タグ 17) と CWT (タグ 61)
  - CAT のクレーム (RFC 8392 の `iss` / `sub` / `aud` / `exp` / `nbf` / `iat` / `cti` と RFC 8747 の `cnf`、IANA 登録の CAT クレームキー)
  - 直列化は COSE 形式 (COSE_Sign1 / COSE_Mac0、CWT タグは任意) と、旧 draft-01 の compact 形式、URL 埋め込み用の Base64
  - 発行 (署名) と検証。アルゴリズムは HMAC 256/384/512、ES256/384/512、EdDSA (Ed25519)
  - draft-02 付録 A のテストベクタ (A.2 の CBOR、A.3 のトークン構造、A.4 の DPoP バインディング、A.5 の scope、A.6 の複合クレームを raw として保持、A.7 の検証) をテストで固定
- `moqt` クレーム
  - claim value の CBOR map、キー 0 の scopes、キー 0 以外の未知キーの拒否 (§2.1.1)
  - アクション 1〜9 (§2.1 Table 1)。未知のアクション整数は保持するが、既知のアクションの許可には使わない
  - exact / prefix / suffix マッチと `nil` の末尾固定、複数スコープの OR 評価
  - `CatClaims::authorize` による scope 認可
- `moqt-reval` (暫定 claim label -65538) のデコードと値の保持。再検証の実行と拒否の判断は利用側の責務
- `catreplay` の 0 / 1 / 2 のデコード。1 の接続束縛は `BearerReplayCache` と `CatToken::verify_bearer` で扱う
- DPoP
  - `cnf` の `ckt` (RFC 9679 の COSE Key Thumbprint、confirmation key 5) と `jkt` (RFC 7638 の JWK サムプリント、confirmation key 323)
  - `catdpop` (受理ウィンドウ / リプレイ保護の有無)
  - 旧 JWT 形式 (JWS compact) の DPoP proof の検証と発行。互換 API であり、draft-02 の標準形式ではない
  - CWT 形式 (`typ` = `dpop-proof+cwt`) の proof のデコードと発行 (`CwtDpopProofCodec`)、署名、鍵束縛、actx の個別検証
  - CWT 形式の proof を CAT トークンとまとめて受理判定する `CwtDpopProof::verify_against_cat_token`
  - `cti` によるリプレイ保護 (`CwtDpopReplayCache`) と `jti` による旧形式のリプレイ保護 (`DpopReplayCache`)
- 暗号
  - `CoseCrypto` trait に署名 / 検証 / ハッシュを分離し、既定ビルドは暗号実装を一切リンクしない
  - `aws-lc-rs` feature で aws-lc-rs を使う実装を提供する (no_std ターゲットでは使えない)

## 検証 API の責務

「デコードできる」「署名が正しい」「認可できる」は別のことです。
このモジュールの API は、次のように責務を分けています。

| API | 検証するもの | 検証しないもの |
| --- | --- | --- |
| `CatToken::decode` | 直列化とクレームの構文 | 署名、時刻、認可 |
| `CatToken::verify` | 署名 / MAC のみ | クレーム、scope、`catreplay`、DPoP |
| `CatToken::verify_with` | 署名 / MAC と、`VerifyOptions` を指定した場合の `alg` / `typ` | クレーム、scope、`catreplay`、DPoP |
| `CatClaims::validate` | `exp` / `nbf` / `iss` / `aud` と数値の有限性 (stateless) | 署名、scope、`catreplay`、DPoP、`moqt-reval` の再検証 |
| `CatClaims::authorize` | `moqt` クレームの scope | 署名、時刻、`catreplay`、DPoP |
| `CatToken::verify_bearer` | `cnf` を持たないこと (あれば拒否)、署名、クレーム、scope、`catreplay` 1 の接続束縛 (2 は拒否) | DPoP (`cnf` 付きトークンは拒否)、`moqt-reval` の再検証 |
| `CwtDpopProof::verify_against_cat_token` | CAT の署名、クレーム、scope と、proof の署名、鍵束縛、`ath`、`actx`、鮮度、`cti` replay | `moqt-reval` の再検証、proof の運搬 |
| `DpopProof::verify_against_cat_token` (旧 JWT 形式) | proof の署名、`ath`、鍵束縛、`actx`、鮮度、`jti` replay | CAT の署名、クレーム、scope (呼び出し側で別途検証する) |

- `CatToken::verify` は署名だけを検証します。`CatToken::verify` が成功しても、action を認可できるとは限りません
- `CatClaims::validate` はクレームだけを見る stateless な検証です。接続束縛も DPoP も検証しません
- `CatClaims::authorize` は `moqt` クレームだけを評価します。トークンの署名や有効期限は検証しません
- bearer トークン (DPoP を使わないトークン) は `CatToken::verify_bearer` を使ってください。`verify` と `validate` と `authorize` を個別に呼ぶだけでは、`catreplay` 1 の別接続での再利用を検出できません
- `cnf` 付きトークンは proof なしで認可してはいけません。`verify_bearer` は `cnf` 付きトークンを拒否します。`CwtDpopProof::verify_against_cat_token` は proof を必須とし、`cnf` が無いトークンも拒否します
- `CwtDpopProof::verify_signature` / `verify_key_binding` / `verify_authorization_context` / `verify_freshness` は個別の条件だけを検証します。これらを個別に呼ぶだけでは受理判定になりません。受理判定には `CwtDpopProof::verify_against_cat_token` を使ってください
- 旧 JWT 形式の `DpopProof::verify_against_cat_token` は proof 側だけを検証します。CAT の署名、クレーム、scope は検証しないため、呼び出し側が別途検証する必要があります
- 時計、I/O、接続識別子、replay cache の所有と、`moqt-reval` に基づく再検証の実行は利用側の責務です
- replay cache は同じトークン / proof を受理する検証主体が共有します。接続ごとに作り直すと、別接続での再利用を検出できません

## CBOR (RFC 8949)

`c4m::cbor` は CWT / COSE / CAT が必要とする範囲の CBOR を実装しています。

### データ項目

`cbor::Value` がすべてのデータ項目を表します。

| variant | major type | 内容 |
| --- | --- | --- |
| `Unsigned(u64)` | 0 | 符号なし整数 |
| `Negative(u64)` | 1 | 負の整数 (`-1 - n`) |
| `ByteString(Vec<u8>)` | 2 | バイト文字列 |
| `TextString(String)` | 3 | テキスト文字列 |
| `Array(Vec<Value>)` | 4 | 配列 |
| `Map(Vec<(Value, Value)>)` | 5 | マップ |
| `Tag(u64, Box<Value>)` | 6 | タグ付きデータ項目 |
| `Bool` / `Null` / `Undefined` / `Float(f64)` / `Simple(u8)` | 7 | 単純値と浮動小数点数 |

### 決定論的エンコード

`cbor::encode` は RFC 8949 §4.2 の決定論的エンコードに従います。

- 整数、長さ、タグは最短形を使います
- マップのキーはキーのエンコード済みバイト列の昇順に並べます (§4.2.1)
- 浮動小数点数は値を保つ最短の幅 (半精度、単精度、倍精度) を使います (§4.2.1)
- NaN は `f9 7e 00` に正規化します (§4.2.2)

整数値の浮動小数点数 (`300.0` など) はそれ自身が最短の表現であるため `f9 5c b0` のように半精度でエンコードされます。
整数としてエンコードしたい場合は `Value::Unsigned` を使ってください。

```rust
use shiguredo_moqt::c4m::cbor::{self, Value};

let value = Value::Map(vec![
    (Value::integer(1), Value::TextString(String::from("https://auth.example.com"))),
    (Value::integer(4), Value::Unsigned(1_700_086_400)),
]);
let bytes = cbor::encode(&value).expect("エンコードできます");
assert_eq!(cbor::decode(&bytes).expect("デコードできます"), value);
```

### デコードの制限

- definite 長と indefinite 長の両方を受理します。break の単独出現と indefinite チャンクの型不一致は拒否します
- ネスト深度は `MAX_DEPTH` (64) までです。超過は `DepthLimitExceeded` です
- マップの重複キーは拒否します。`-0.0` と `0.0` は RFC 8949 §5.6.1 の等価規則どおり同一として扱います。NaN は実装判断で、シグニフィカンドが異なる場合もすべて同一として扱います (RFC 8949 §5.6.1 はシグニフィカンドを 64 bit にゼロ拡張して一致する場合だけ等価とするため、本ライブラリの扱いはそれより広い)
- テキスト文字列は UTF-8 であることを検証します。indefinite 長はチャンクごとに検証します (§3.2.3)
- 予約された additional information (28〜30)、予約された単純値 (24〜31)、末尾の余分なバイトは拒否します
- 複数のデータ項目を読む場合は `decode_partial` を使います。消費したバイト数を返します

### テスト

draft-02 付録 A.2 の 8 ベクタ (decode) と、compact 側の 7 ベクタ (decode と再エンコードの一致)、境界値 (整数の各幅、f16 / f32 / f64、indefinite、深度、重複キー、UTF-8)、PBT のラウンドトリップ、fuzz ターゲットで固定しています。

## COSE (RFC 9052 / RFC 9053)

`c4m::cose` は COSE_Sign1 と COSE_Mac0、および CWT のタグを扱います。

### 構造

| 構造 | CBOR タグ | 用途 |
| --- | --- | --- |
| `CoseMac0` | 17 | MAC アルゴリズム (HMAC) |
| `CoseSign1` | 18 | 署名アルゴリズム (ECDSA / EdDSA) |
| CWT | 61 | COSE 構造を包む CWT のタグ |

構造はどちらも `[protected, unprotected, payload, signature]` の 4 要素です。
protected は protected ヘッダを CBOR でエンコードしたバイト文字列、unprotected はヘッダのマップです。

RFC 8392 §6 は CWT タグが COSE のタグ付きオブジェクトに前置されることを MUST とするため、CWT タグだけを付けた構造の decode と encode は拒否します。
draft-02 付録 A.3 のトークンは CWT タグを付けない COSE 構造ですが、`CatToken::decode` は CWT タグの有無の両方を受理します。

### ヘッダ

| ラベル | 名前 | 扱い |
| --- | --- | --- |
| 1 | `alg` | protected ヘッダに必須。unprotected にしかない場合は拒否 |
| 2 | `crit` | protected ヘッダのみ。配列は 1 要素以上。ラベルは同じ protected ヘッダに実在し、理解できる必要がある |
| 3 | `content type` | protected と unprotected のどちらでも可 |
| 4 | `kid` | バイト文字列とテキスト文字列の両方を受理 |
| 14 | `kccs` | CWT 形式の DPoP proof が確認用の COSE_Key を運ぶ CWT Claims Set (RFC 9528) |
| 16 | `typ` | protected ヘッダのみ (RFC 9596 §2) |

同じラベルが protected と unprotected の両方にある場合は `DuplicateHeaderParameter` で拒否します。
未知のヘッダパラメータは `Header::raw` に保持します。`crit` が指す counter signature (ラベル 7) は理解できないものとして拒否します (fail-closed)。

### 署名対象

署名と MAC の対象は RFC 9052 の構造化バイト列です。external_aad は常に空です。

```text
Signature1 = ["Signature1", protected (bstr), external_aad (bstr, 空), payload (bstr)]
MAC0       = ["MAC0",       protected (bstr), external_aad (bstr, 空), payload (bstr)]
```

compact 形式のトークンは COSE 構造ではないため、署名対象は base64url のままの `protected.payload` (ASCII) です。詳細は「CAT」の節を参照してください。

### アルゴリズム

| アルゴリズム | COSE 識別子 | 曲線 |
| --- | --- | --- |
| HMAC 256/256 | 5 | - |
| HMAC 384/384 | 6 | - |
| HMAC 512/512 | 7 | - |
| ES256 | -7 | P-256 (secp256r1) |
| ES384 | -35 | P-384 (secp384r1) |
| ES512 | -36 | P-521 (secp521r1) |
| EdDSA | -8 | Ed25519 |

- 鍵は `CoseKey` の `Symmetric`、`Ec2`、`Okp` で表します。EC2 の座標と秘密鍵はビッグエンディアンの固定長です
- ECDSA の署名は COSE の固定長 r||s (ES256 は 64 バイト) です
- draft-02 のベクタは HMAC-SHA256 に `5` を使います。旧 draft-01 の compact ベクタは `-4` を使いますが、IANA では `-4` は A192KW です。**検証では `-4` と `5` の両方を HMAC-SHA256 として受理**し、発行は `5` を使います (`C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID`)
- RSA (PS256 / RS256 など) は未対応です。未知のアルゴリズムは `UnsupportedAlgorithm` になります

```rust
use shiguredo_moqt::c4m::cose::{Algorithm, CoseEncodingOptions, CoseMessage};
use shiguredo_moqt::c4m::crypto::CoseKey;

// 構造のデコード (CWT タグと COSE タグを許容します)
let message = CoseMessage::decode(&bytes)?;
let header = message.header()?; // protected と unprotected を統合したヘッダ
assert_eq!(header.algorithm, Some(Algorithm::HmacSha256));

// 署名対象バイト列の取得
let signing_input = message.signing_input()?;
```

### テスト

タグの組み合わせ、アルゴリズムとタグの整合、ヘッダのバケット規則、Sig_structure と MAC_structure のバイト列、エラー経路をテストで固定しています。

## CWT (RFC 8392)

`CatClaims` が CWT のクレームセットです。

### クレーム

| claim key | 名前 | 型と補足 |
| --- | --- | --- |
| 1 | `iss` | テキスト文字列。`validate` の `expected_issuers` と照合 |
| 2 | `subject` | テキスト文字列 |
| 3 | `aud` | テキスト文字列、またはテキスト文字列の配列。`validate` の `expected_audiences` と照合 |
| 4 | `exp` | 整数または浮動小数点数 (UNIX 秒) |
| 5 | `nbf` | 整数または浮動小数点数 (UNIX 秒) |
| 6 | `iat` | 整数または浮動小数点数 (UNIX 秒) |
| 7 | `cti` | バイト文字列、またはテキスト文字列 |
| 8 | `cnf` | マップ (RFC 8747)。後述 |

- `exp` の `tolerance` は `clock_tolerance_seconds` で指定します。`exp + tolerance` より現在時刻が大きければ `Expired`、`nbf` より現在時刻が小さければ `NotYetValid` です
- 非有限値 (NaN / 無限大) の数値クレームは decode の時点で拒否します。手組みのクレームは `validate` が拒否します
- claim key は整数とテキスト文字列を受理します。それ以外の型は `UnexpectedType` で拒否します
- 型付きフィールドを持つ claim key を `raw` に置いたクレームセットは encode で拒否します (decode との解釈のずれを防ぐため)

```rust
use shiguredo_moqt::c4m::cat::ClaimValidationOptions;

token.claims().validate(&ClaimValidationOptions {
    reference_time_seconds: 1_700_000_000.0,
    clock_tolerance_seconds: 60.0,
    expected_issuers: &["https://auth.example.com"],
    expected_audiences: &["https://relay.example.com"],
})?;
```

### 未解釈のクレーム

CAT 固有のクレームはクレームキーの定数だけを公開し、値は未解釈のまま `CatClaims::raw` に保持します。
値を読む場合は `CatClaims::get` に定数を渡します。

```rust
use shiguredo_moqt::c4m::cat::{CLAIM_CAT_URI, CLAIM_CAT_VERSION};

let version = claims.get(CLAIM_CAT_VERSION);
let uri_restriction = claims.get(CLAIM_CAT_URI);
```

## CAT (CTA-5007-B)

### トークンの直列化

| 形式 | 表現 | 署名対象 | 位置付け |
| --- | --- | --- | --- |
| COSE | COSE_Sign1 / COSE_Mac0 の CBOR (CWT タグの有無は問わない) | `Sig_structure` / `MAC_structure` | draft-02 の標準形式 |
| compact | `base64url(protected).base64url(claims).base64url(signature)` | ASCII の `protected.claims` | 旧 draft-01 の互換形式 |
| URL 埋め込み | 標準 Base64 (RFC 4648 §4)、または base64url のテキスト (§2 / §5) | 形式による | URL へ載せる表現 |

`CatToken::decode` は compact 形式、COSE 形式の CBOR、base64url または標準 Base64 で包んだテキストの順に自動判別します。
パディングの有無はどちらも受理します。

draft-02 §8.2.1 は CAT を CBOR エンコードした CWT と定めており、付録 A.3 のベクタは
CWT タグを付けない COSE 構造の base64url です。RFC 8392 §6 は CWT タグを任意としている
ため、この 2 つは矛盾しません。本ライブラリは CWT タグの有無の両方をデコードでき、
`build_cose` は既定で CWT タグと COSE タグの両方を付けます。付録 A.3 と同じ形にする
場合は `CoseEncodingOptions { cwt_tag: false, ..Default::default() }` を `build_cose_with`
に渡してください。

compact 形式は旧 draft-01 のベクタの形式であり、draft-02 の標準形式ではありません。旧形式との相互運用が必要な場合だけ `build_compact` / `decode_compact` を使ってください。

### claim key の一覧

| claim key | 名前 | 値の型 (IANA 登録) |
| --- | --- | --- |
| 308 | `catreplay` | 符号なし整数 |
| 309 | `catpor` | 配列 |
| 310 | `catv` | 符号なし整数 |
| 311 | `catnip` | 配列 |
| 312 | `catu` | マップ |
| 313 | `catm` | 配列 |
| 314 | `catalpn` | 配列 |
| 315 | `cath` | マップ |
| 316 | `catgeoiso3166` | 配列 |
| 317 | `catgeocoord` | 配列 |
| 318 | `catgeoalt` | 配列 |
| 319 | `cattpk` | バイト文字列 |
| 320 | `catifdata` | 文字列または配列 |
| 321 | `catdpop` | マップ |
| 322 | `catif` | マップ |
| 323 | `catr` | マップ |
| -65537 | `moqt` | マップ (暫定 label) |
| -65538 | `moqt-reval` | 数値 (暫定 label) |

`moqt` / `moqt-reval` / `catdpop` / `catreplay` は型付きで解釈します。`catu` の URI マッチ評価など、型付きで解釈しない CAT クレームはキーと値の保持だけを行います。

`moqt` と `moqt-reval` の claim label は draft-02 §8 Table 2 でも `TBD_MOQT (1+2)` のままで、`moqt-reval` は §2.2 の本文でも `TBD_MOQT_REVAL` のままです。IANA の割当は未確定です。
本ライブラリは付録 A 冒頭のとおり、RFC 8392 §9.1.1 の Private Use の -65537 / -65538 をプレースホルダとして使います。
正式な割当ではありません。IANA の割当が確定した時点で置き換えます。
旧 draft-01 のベクタが使う 327 / 328 は -65537 / -65538 の alias として解釈しません。327 / 328 をキーに持つクレームは未知のクレームとして `raw` に保持され、型付きフィールドへは写りません。

### `moqt` クレーム (暫定 claim label -65537)

draft-02 §2.1 の `moqt` クレームは CBOR map です。キー 0 にスコープの配列を持ちます。

```text
moqt-value = { moqt-scopes-key => moqt-scopes, * int => any }
moqt-scopes-key = 0
moqt-scopes = [ + moqt-scope ]
moqt-scope = [ moqt-actions, ? [ + moqt-ns-match ], ? moqt-track-match ]
moqt-actions = [ + moqt-action ]
moqt-action = int
moqt-ns-match = bin-match / nil
moqt-track-match = bin-match
bin-match = bstr / [ match-type, match-value ]
match-type = 1 (prefix) / 2 (suffix)
```

キー 0 以外のキーは将来の改訂のために予約されています (§2.1.1)。
未知のキーを含む `moqt` クレームは、認可範囲を狭める将来のキーを取りこぼさないよう、fail-closed でその claim set を受理しません。
`moqt` クレーム全体が map でない場合、キー 0 が無い場合、キー 0 の値が配列でない場合、scopes が空の場合も拒否します。
キー 0 が重複する map は CBOR のキー一意性に反するため拒否します。

| アクション | key |
| --- | --- |
| SETUP | 1 |
| PUBLISH_NAMESPACE | 2 |
| SUBSCRIBE_NAMESPACE | 3 |
| SUBSCRIBE | 4 |
| REQUEST_UPDATE | 5 |
| PUBLISH | 6 |
| FETCH | 7 |
| TRACK_STATUS | 8 |
| SUBSCRIBE_TRACKS | 9 |

旧 draft-01 の CLIENT_SETUP=0 / SERVER_SETUP=1 という 2 つのアクションは、draft-02 で SETUP=1 に統合されました。
`MoqtAction::from_key(0)` は `None` を返し、0 は既知のアクションの alias にしません。
スコープに未知のアクション整数が入っていても、既知のアクションの許可としては働きません。

マッチの規則は次のとおりです。

- バイト文字列は完全一致、`[1, value]` は前方一致、`[2, value]` は後方一致です
- 名前空間のマッチはフィールド単位で行います。`nil` は名前空間の末尾に一致することを要求し、末尾以外に置くと不正です
- 名前空間マッチの末尾に `nil` が無い場合は、マッチしたフィールド以降の名前空間フィールドを任意として受理します
- トラック名のマッチは省略できます。省略した場合はすべてのトラック名に一致します
- 名前空間マッチとトラック名マッチは省略できますが、トラック名マッチだけを持つスコープは表現できないため encode を拒否します

```rust
use shiguredo_moqt::c4m::{Match, MoqtAction, MoqtClaim, MoqtScope, NamespaceMatch};

// 名前空間 ["example.com", "alice"] の "video-" で始まるトラックを PUBLISH できる
let scope = MoqtScope::new([MoqtAction::Publish])
    .namespace_match(NamespaceMatch::Match(Match::Exact(b"example.com".to_vec())))
    .namespace_match(NamespaceMatch::Match(Match::Exact(b"alice".to_vec())))
    .track(Match::Prefix(b"video-".to_vec()));
```

スコープが複数ある場合、いずれかのスコープが一致すれば認可されます。評価順は問いません。
`CatClaims::authorize` は `moqt` クレームが無ければ常に `false` を返します (明示的に許可されたアクション以外はブロックします)。
`moqt` クレームが無いトークンは、他のクレームが正しくても MOQT のアクションを認可しません。

### `moqt-reval` クレーム (暫定 claim label -65538)

再検証の間隔 (秒) です (§2.2)。

- `0` の場合は再検証しません。クレームが無い場合も同じです
- 非ゼロの場合は、継続中のストリームに対して再検証を要求します。トークンが受理できなくなったら、認可されたアクションを継続してはいけません
- 受信者が再検証できない場合、受信者が用意できる間隔より再検証間隔が小さい場合は、そのトークンを拒否する MUST があります
- クレームが composition claims の中にあるトークンは well-formed ではありません。本ライブラリは複合クレームを評価しないため、この well-formed 性検査は行いません。複合クレームの中にしか `moqt` が無いトークンは、`moqt` が読めず全アクションがブロックされます

本ライブラリは Sans-I/O のため再検証を実行しません。
`CatClaims::validate` は `moqt-reval` の値の有限性だけを検査し、非ゼロの `moqt-reval` を既定では拒否しません。
そのため、`CatClaims::validate` が成功したことを「非ゼロの `moqt-reval` 付きトークンを受理してよい」と解釈してはいけません。
利用側は `claims().moqt_reval` を確認し、再検証できない場合は非ゼロの値を拒否し、再検証できる場合はその間隔で再検証してください。
再検証を実行しないアプリケーションが `validate` だけを呼ぶと、この MUST を満たせません。

### `catreplay` クレーム (claim key 308)

- `0` または省略: リプレイ保護を行いません
- `1`: 単一接続での再利用だけを許します (§3)。同じトークンを SETUP と後続のアクションで同じ接続に提示できますが、別の接続では拒否します。`cti` と `exp` が必須です。接続が終了した後も、トークンが受理され得る間 (期限の leeway を含む) は replay state を保持する MUST があります
- `2`: リプレイの意味論は実装定義です (§3)。本ライブラリの bearer 検証は未対応として拒否し、0 と同じ扱いにはしません

`catreplay` の接続束縛は `CatToken::verify_bearer` と `BearerReplayCache` が扱います。
キャッシュは受信者が所有し、その受信者が受理するトークン群と全接続で共有します。接続ごとに作り直してはいけません。
接続識別子はアプリケーションが発行する opaque な値で、記録が残っている間は使い回さないでください。
未失効の記録を追い出して別接続での再利用を許すことはしません。
複数のリレー間で束縛を共有する場合は、アプリケーションが状態を共有する必要があります (§7.2)。§7.2 は共有を SHOULD とし、DPoP の使用を代替としています。

`CwtDpopReplayCache` は proof の `cti` 用であり、bearer トークンの接続束縛用の `BearerReplayCache` の代用にはなりません。逆も同じです。
両者はキーも保持期間も意味も異なります。

### DPoP バインディング (`cnf` と `catdpop`)

- `cnf` の `ckt` は COSE Key Thumbprint (confirmation key 5、RFC 9679) です。RFC 9679 は SHA-256 のサポートを MUST とし、他のハッシュを MAY とします。本ライブラリは SHA-256 の 32 バイトだけを受理します。draft-02 §4.1 は `ckt` の使用を推奨し、リレーに `ckt` のサポートを MUST、`jkt` のサポートを SHOULD としています
- `cnf` の `jkt` は JWK サムプリント (confirmation key 323、RFC 7638) で、既存の CAT 発行者との互換のために MAY で認められています
- `ckt` と `jkt` が併存する場合、CWT 形式の proof の鍵束縛では両方の一致を要求します。片方だけの一致では受理しません
- `cnf` に `ckt` も `jkt` も無い場合は鍵束縛を確認できないため、CWT 形式の proof では拒否します
- `cnf` の confirmation key 3 は IANA では `kid` です。旧 draft-01 のベクタが `jkt` として使っていますが、CWT 形式の鍵束縛では解釈せず、`kid` ベースの PoP と区別します
- `catdpop` (claim key 321) は label 0 が受理ウィンドウ (秒)、label 1 がリプレイ保護の有無です (§4.1)。CWT 形式の proof は `cti` を、旧 JWT 形式の proof は `jti` を使います
- `cnf` 付きトークンは、MOQT のアクションに有効な DPoP proof が伴わなければ受理できません (§4.1)。proof なしの `cnf` 付きトークンを `verify_bearer` や `CatClaims::authorize` だけで認可してはいけません

### 発行と検証

```rust
use shiguredo_moqt::c4m::cat::{
    BearerReplayCache, BearerVerificationOptions, CatReplay, CatToken, CatTokenBuilder,
    VerifyOptions, CAT_CONTENT_TYPE,
};
use shiguredo_moqt::c4m::cose::Algorithm;
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;
use shiguredo_moqt::c4m::crypto::CoseKey;
use shiguredo_moqt::c4m::{Match, MoqtAction, MoqtClaim, MoqtScope, NamespaceMatch};

let crypto = AwsLcRsCrypto::new();
let key = CoseKey::symmetric(shared_secret);

// 発行 (認可サーバー側)。標準の COSE 形式を返す
let token_bytes = CatTokenBuilder::new()
    .issuer("https://auth.example.com")
    .audience("https://relay.example.com")
    .issued_at(unix_now)
    .expiration(unix_now + 3600.0)
    .cwt_id(b"token-001".to_vec())
    .moqt(
        MoqtClaim::new().scope(
            MoqtScope::new([MoqtAction::Subscribe, MoqtAction::Fetch])
                .namespace_match(NamespaceMatch::Match(Match::Exact(b"example.com".to_vec())))
                .track(Match::Prefix(b"video-".to_vec())),
        ),
    )
    .cat_replay(CatReplay::SingleConnection)
    .build_cose(&crypto, &key)?;

let token = CatToken::decode(&token_bytes)?;
token.verify_with(&crypto, &key, &VerifyOptions {
    expected_algorithm: Some(Algorithm::HmacSha256),
    expected_type: Some(CAT_CONTENT_TYPE),
})?;

// bearer トークンの接続束縛まで検証する
let mut cache = BearerReplayCache::new(60.0)?;
token.verify_bearer(&crypto, &key, &BearerVerificationOptions {
    reference_time_seconds: unix_now,
    expected_issuers: &["https://auth.example.com"],
    expected_audiences: &["https://relay.example.com"],
    verify_options: VerifyOptions::default(),
    action: MoqtAction::Subscribe,
    namespace: &[b"example.com"],
    track_name: b"video-live",
    connection_id: connection_id,
}, &mut cache)?;
```

- `CatTokenBuilder` はクレームと `moqt` スコープを組み立て、`build_cose` / `build_cose_with` で標準の COSE 形式、`build_compact` で旧 draft-01 の compact 形式を発行します
- 署名アルゴリズムは鍵の種別から自動選択します (対称鍵は HMAC-SHA256、EC2 は ES256 / ES384 / ES512、OKP は EdDSA)。`algorithm` で明示もできます
- `typ` は既定で `CAT` です。`VerifyOptions::expected_type` に `CAT_CONTENT_TYPE` を渡すと `typ` を検証できます
- COSE 形式の発行は CWT タグと COSE タグを付与します。タグの付与は `CoseEncodingOptions` で制御できます
- 発行するクレームの数値が有限でない場合、および型付きキーを `raw` に置いた場合はエラーを返します
- `verify` は署名だけを、`verify_with` は `VerifyOptions` を指定した場合に `alg` / `typ` も検証します。クレームの検証には `CatClaims::validate`、scope の認可には `CatClaims::authorize` が必要です
- `verify_bearer` は `cnf` 付きトークンを拒否します。`cnf` 付きトークンは `CwtDpopProof::verify_against_cat_token` で proof と合わせて検証してください

### MOQT の Auth Token Type

`MOQT_AUTH_TOKEN_TYPE_CAT` が `0x01` です (§8.2.1)。
Token Value からデコードする場合は `CatToken::decode_moqt_auth_token` を使います。

```rust
use shiguredo_moqt::c4m::cat::{CatToken, MOQT_AUTH_TOKEN_TYPE_CAT};

let token = CatToken::decode_moqt_auth_token(MOQT_AUTH_TOKEN_TYPE_CAT, token_value)?;
```

`decode_moqt_auth_token` もデコードだけを行います。署名、クレーム、scope の検証は別の API で行ってください。
MOQT の AUTHORIZATION_TOKEN に CWT 形式の DPoP proof をどう載せるかは draft-02 でも定義されていません。独自の envelope を割り当てず、proof の運搬方法は利用側のプロトコル設計に委ねてください。

### 仕様側の未確定事項

実装は次の仕様の揺れを吸収しています。確定したら追従が必要です。

- トークンの直列化: §8.2.1 は CBOR の CWT と定めますが、RFC 8392 §6 は CWT タグを任意としています。本ライブラリは CWT タグの有無の両方をデコードし、発行はタグ付きを既定にしています
- `moqt` / `moqt-reval` の claim label: §8 Table 2 も `TBD_MOQT (1+2)`、§2.2 も `TBD_MOQT_REVAL` で、付録 A は Private Use の -65537 / -65538 をプレースホルダにしています
- `actx` / `ath` の claim label: §4.2 の CDDL でも `TBD_ACTX` / `TBD_ATH` です。利用側が label を指定します
- HMAC-SHA256 の識別子: draft-02 のベクタは `5`、旧 draft-01 の compact ベクタは `-4` です。検証は両方、発行は `5` です
- `cnf` の `jkt` と key 3: 旧 draft-01 のベクタは confirmation key 3 を `jkt` に使いますが、IANA では `kid` です。旧 JWT 形式の検証だけが互換のために 3 を参照します
- 付録 A.5 の `moqt_admin_wildcard`: actions が `0 〜 8` で action 0 の認可期待値が true のままで、§2.1 Table 1 (SETUP=1 〜 SUBSCRIBE_TRACKS=9) と不整合です。本ライブラリは action 0 を既知 action にせず認可しません
- 付録 A.6 の複合クレーム: claim key がすべて暫定の 0 で、wire だけでは演算子 (or / and / nor) を区別できません。本ライブラリは raw として保持し、認可には使いません

### 残る制約

- draft-lemmons-cose-composite-claims の複合論理クレーム (`or` / `and` / `nor` など) は
  評価しません。`CatClaims::raw` に保持するだけです。複合クレームの中の `moqt` は
  読めないため、複合クレームだけを持つトークンはどのアクションも認可されません。
  複数スコープを並べる `moqt` クレーム内の OR には対応しています
- top-level の `moqt` と複合クレームが併存するトークンでは、top-level の `moqt` だけで
  認可されます。複合クレームによる制限を評価したい場合は、複合クレームを含むトークンを
  受理しないでください (fail-closed)。付録 A.6 自体も、複合クレームの key 0 を有効な
  claim key としない前提で書かれています
- CWT 形式の DPoP proof を MOQT の AUTHORIZATION_TOKEN に載せる方法は仕様で未定義です。ライブラリは proof の codec と検証だけを提供し、運搬用の envelope を独自に定義しません
- `catreplay` 2 は実装定義ですが、本ライブラリの bearer 検証では未対応として拒否します
- 非ゼロの `moqt-reval` は、既定の `CatClaims::validate` では拒否されません。利用側が再検証するか拒否してください
- `cnf` と `catreplay` 1 / 2 が併存するトークンは、両者の意味が未定義のため `CwtDpopProof::verify_against_cat_token` では受理しません
- 旧 JWT 形式の `DpopReplayCache` は、現在の proof の `catdpop` ウィンドウで古い記録を破棄します。ウィンドウの異なるトークンで同じキャッシュを共有すると、記録が早く消えて `jti` を再受理し得ます。旧形式の互換 API を使う場合は、トークンごとにキャッシュを分けるか、最大ウィンドウ以上の運用をしてください
- CTA-5007-B の HTTP 向けクレームの意味論 (`catu` のマッチ評価、`catr` の更新など)
- RSA (PS256 / RS256 など)

旧 JWT / compact / key 3 の API は互換のために残しています。新しい実装では COSE 形式のトークンと CWT 形式の proof を使ってください。

## DPoP

DPoP はトークンをクライアントの秘密鍵に束縛し、盗まれたトークンの再利用を防ぎます。
draft-02 の標準は CWT 形式の proof (§4.2) です。
旧 JWT 形式 (draft-nandakumar-moq-generic-dpop-proof-00) の proof は互換 API として残しています。

### CWT 形式 (§4.2)

proof は COSE_Sign1 で、protected ヘッダに `alg` / `typ` / `kccs` を持ちます。

- `alg` (ラベル 1): 非対称署名アルゴリズム。対称アルゴリズムは使えません
- `typ` (ラベル 16): `dpop-proof+cwt`
- `kccs` (ラベル 14): CWT Claims Set。`cnf` (8) にクライアントの公開鍵を COSE_Key (1) として入れます

ペイロードの CWT Claims Set は次のクレームを持ちます。

```text
dpop-proof-claims = {
  7 => bstr,               ; cti: proof の一意な識別子
  6 => int,                ; iat: 作成時刻
  actx-label => moqt-actx, ; actx: 認可する MOQT アクション
  ath-label => bstr,       ; ath: CAT トークンの SHA-256
}

moqt-actx = {
  0 => "moqt",             ; type
  1 => moqt-action,        ; action
  ? 2 => [ * bstr ],       ; Track Namespace のフィールド
  ? 3 => bstr,             ; Track Name
}

actx-label = TBD_ACTX
ath-label = TBD_ATH
```

- `actx` の action は `moqt` クレームと同じアクションコードです
- `actx` の namespace / track は MOQT のメッセージが運ぶバイト列をそのまま入れます。シリアライズも正規化もしません
- 本ライブラリでは namespace / track の要否を action ごとに定めています。SETUP はどちらも持たず、PUBLISH_NAMESPACE / SUBSCRIBE_NAMESPACE / SUBSCRIBE_TRACKS は namespace だけ、SUBSCRIBE / REQUEST_UPDATE / PUBLISH / FETCH / TRACK_STATUS は両方を要求します。欠落と空の値は区別します。REQUEST_UPDATE は MOQT のメッセージに FQN を持ちませんが、本ライブラリでは track 更新を対象として両方を要求します
- `actx` の `type` は `moqt` 固定です。他のプロトコル向けの proof を流用できないようにします
- `ath` はリレーに提示された CAT トークンのバイト列そのものの SHA-256 です。再エンコードや Base64 の正規化をした表現ではありません。`CatToken::raw_token()` が返すバイト列を使います

`actx` と `ath` の claim label はドラフトでも TBD のため、ライブラリは値を割り当てません。
呼び出し側が label を指定して `CwtDpopProofCodec::new(actx_label, ath_label)` を作り、発行、デコード、検証のすべてで同じ設定を使ってください。
`actx` と `ath` が同じ値になる設定、および CWT の登録済み claim key (1〜8) と衝突する設定は拒否します。
`verify_against_cat_token` は、proof をデコードした codec と検証に渡した codec の label が一致することも確認します。
`CwtDpopProofCodec::build` は CWT タグを付けず、COSE_Sign1 タグ (18) だけを付けます。

```rust
use shiguredo_moqt::c4m::cat::{
    CatToken, ClaimValidationOptions, VerifyOptions, CAT_CONTENT_TYPE,
};
use shiguredo_moqt::c4m::cose::Algorithm;
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;
use shiguredo_moqt::c4m::crypto::{CoseCrypto, CoseKey, DigestAlgorithm};
use shiguredo_moqt::c4m::dpop::{
    CwtAuthorizationContext, CwtDpopProofClaims, CwtDpopProofCodec, CwtDpopReplayCache,
    CwtDpopVerification, MOQT_AUTHORIZATION_CONTEXT_TYPE,
};
use shiguredo_moqt::c4m::MoqtAction;

// 未定義の変数は利用側の文脈で用意する疑似コードである
let crypto = AwsLcRsCrypto::new();

// actx / ath の claim label は TBD。呼び出し側が決めた値で codec を作る
let codec = CwtDpopProofCodec::new(actx_label, ath_label)?;

// 発行: ath は提示する CAT トークンのバイト列の SHA-256
let token = CatToken::decode(&token_bytes)?;
let ath = crypto.digest(DigestAlgorithm::Sha256, token.raw_token())?;
let proof_bytes = codec.build(&crypto, &client_key, &CwtDpopProofClaims {
    cti: cti_bytes,
    issued_at: unix_now as i64,
    authorization_context: CwtAuthorizationContext {
        context_type: String::from(MOQT_AUTHORIZATION_CONTEXT_TYPE),
        action: MoqtAction::Subscribe.key(),
        namespace: Some(vec![b"example.com".to_vec()]),
        track_name: Some(b"video-live".to_vec()),
        raw: Vec::new(),
    },
    access_token_hash: ath,
    raw: Vec::new(),
})?;

// 検証: CAT の署名、クレーム、scope と proof の全条件を 1 つの入口で判定する
let namespace: &[&[u8]] = &[b"example.com"];
let proof = codec.decode(&proof_bytes)?;
let mut cache = CwtDpopReplayCache::new(300.0)?;
proof.verify_against_cat_token(&crypto, &CwtDpopVerification {
    token: &token,
    token_key: &token_key,
    token_verify_options: VerifyOptions {
        expected_algorithm: Some(Algorithm::HmacSha256),
        expected_type: Some(CAT_CONTENT_TYPE),
    },
    claim_options: ClaimValidationOptions {
        reference_time_seconds: unix_now,
        clock_tolerance_seconds: 60.0,
        expected_issuers: &["https://auth.example.com"],
        expected_audiences: &["https://relay.example.com"],
    },
    action: MoqtAction::Subscribe,
    namespace: Some(namespace),
    track_name: Some(b"video-live"),
    codec,
    default_window_seconds: 60.0,
}, &mut cache)?;
```

`CwtDpopProof::verify_against_cat_token` は §4.3 の条件を次の順で検証します。すべて成功したときだけ `cache` に replay 状態を記録します。

1. `default_window_seconds` が有限で 0 以上であること
2. CAT の署名 / MAC
3. CAT のクレーム (`exp` / `nbf` / `iss` / `aud`)
4. proof のクレームが codec の label 設定と一致すること
5. proof の署名 (`kccs` の公開鍵)
6. `kccs` の公開鍵と `cnf` の `ckt` / `jkt` の一致
7. `ath` と提示された CAT のバイト列の SHA-256 の一致
8. `actx` の `type` / action / namespace / track と要求の一致
9. `iat` の鮮度 (`catdpop` の label 0、無ければ既定のウィンドウ)
10. `moqt` クレームによる scope 認可
11. `cnf` と `catreplay` 1 / 2 が併存するトークンの拒否
12. proof のウィンドウが cache の最大ウィンドウを超えないこと
13. `catdpop` が replay detection を要求している場合の `cti` の確認と記録

鮮度は `iat` と参照時刻の差の絶対値がウィンドウ以下かで判定します (境界を含む)。
replay 検出を要求しない proof も、ウィンドウが cache の最大ウィンドウを超えないことの判定には cache を使うため、cache は省略できません。

`CwtDpopReplayCache` は同じトークン / proof を受理する検証主体が全接続で共有します。
接続ごとに作り直すと、別の接続で同じ `cti` を再利用できてしまいます。
記録は `cti` から保持期限 (`iat` + 最大ウィンドウ) への写像で、未失効の記録を追い出して別の `cti` を記録することはありません。
参照時刻は単調増加を前提とします。
`BearerReplayCache` とはキーも保持期間も意味も異なるため、互いに代用してはいけません。

### 旧 JWT 形式 (互換 API)

旧 draft-01 と draft-nandakumar-moq-generic-dpop-proof-00 の JWT 形式の proof も扱えます。
`typ` は `dpop-proof+jwt` で、ヘッダに埋め込んだ JWK で署名を検証します。
この形式は draft-02 の標準ではありません。互換性のためにだけ使ってください。

`DpopProof::verify_against_cat_token` は次の項目をこの順で検証します。

1. proof の署名 (埋め込み JWK で検証。JWK に秘密鍵がある proof は拒否)
2. `ath` (トークンの `raw_token()` と照合。アクセストークンを伴う proof は `ath` が必須)
3. `cnf` の JWK サムプリントと proof の JWK サムプリントの一致 (confirmation key 323 を優先し、無い場合は旧 draft-01 の key 3)
4. Authorization Context (`actx.type` は `moqt`、action、`tns` / `tn`、`resource` の整合)
5. `iat` の鮮度 (`catdpop` のウィンドウ)
6. `jti` のリプレイ (`catdpop` が jti を要求する場合は `DpopReplayCache` が必須)

```rust
use shiguredo_moqt::c4m::dpop::{DpopProof, DpopReplayCache};

let proof = DpopProof::decode(proof_jwt)?;
let mut replay_cache = DpopReplayCache::new();
proof.verify_against_cat_token(
    &crypto,
    &token,
    MoqtAction::Publish,
    &namespace,
    track_name,
    unix_now,
    60.0,
    Some(&mut replay_cache),
)?;
```

- この API は proof 側だけを検証します。CAT の署名、クレーム、scope は検証しません。呼び出し側が `CatToken::verify` / `CatClaims::validate` / `CatClaims::authorize` を別途実行してください
- `DpopReplayCache` は現在の proof のウィンドウで古い記録を破棄するため、ウィンドウの異なるトークンで同じキャッシュを共有すると `jti` を再受理し得ます
- `cnf` の key 3 は IANA では `kid` です。旧形式の検証だけが互換のために JWK サムプリントとして扱います
- アクセストークンを伴わない proof (token endpoint など) は `verify_against_token` に `DpopVerification { access_token: None, .. }` を渡します
- サーバーが `nonce` を発行している場合の照合はアプリ側で `proof.claims().nonce` を確認してください (`verify_against_cat_token` は検証しません)

### 発行

クライアント側は `DpopProofBuilder` で旧 JWT 形式の proof を発行します。
`AuthorizationContext` の `tns` / `tn` は `name::serialize_namespace` / `name::serialize_track_name` の正規シリアライズを使います。

```rust
use shiguredo_moqt::c4m::dpop::{AuthorizationContext, DpopProofBuilder, MOQT_AUTHORIZATION_CONTEXT_TYPE};
use shiguredo_moqt::c4m::crypto::{CoseCrypto, DigestAlgorithm};
use shiguredo_moqt::name::{serialize_namespace, serialize_track_name};
use base64ct::{Base64UrlUnpadded, Encoding};

// 未定義の変数は利用側の文脈で用意する疑似コードである
let actx = AuthorizationContext {
    context_type: String::from(MOQT_AUTHORIZATION_CONTEXT_TYPE),
    action: String::from(MoqtAction::Publish.authorization_context().expect("PUBLISH has an old action name")),
    track_namespace: serialize_namespace(&namespace),
    track_name: Some(serialize_track_name(track_name)),
    resource: Some(format!("moqt://relay.example.com?tns={}&tn={}", serialize_namespace(&namespace), serialize_track_name(track_name))),
    raw: String::new(),
};

let digest = crypto.digest(DigestAlgorithm::Sha256, token.raw_token())?;
let ath = Base64UrlUnpadded::encode_string(&digest);
let proof_jwt = DpopProofBuilder::new(jti, unix_now, actx)
    .access_token_hash(ath)
    .build(&crypto, &private_key, &jwk)?;
```

- 旧 JWT 形式の署名は非対称アルゴリズムだけです。HMAC は DPoP では使えません
- 埋め込む JWK と署名鍵が同じ公開鍵でない場合は `KeyMismatch` で失敗します
- `AuthorizationContext` の型付きフィールドは `type`、`action`、`tns`、`tn`、`resource` の 5 つです。`raw` にしか無い拡張フィールドは発行時に出力されません
- 旧形式に対応する文字列を持たない SUBSCRIBE_TRACKS は `authorization_context()` が `None` を返します。旧形式の actx では SUBSCRIBE_TRACKS を表現できません

## 対応仕様

- [draft-ietf-moq-c4m-02](https://datatracker.ietf.org/doc/html/draft-ietf-moq-c4m-02) - Authorization scheme for MOQT using Common Access Tokens
- [draft-nandakumar-moq-generic-dpop-proof-00](https://datatracker.ietf.org/doc/draft-nandakumar-moq-generic-dpop-proof/) - Application-Agnostic Demonstration Proof of Possession (DPoP) Framework (旧 JWT 形式の互換 API)
- [RFC 8949](https://www.rfc-editor.org/rfc/rfc8949) - Concise Binary Object Representation (CBOR)
- [RFC 9052](https://www.rfc-editor.org/rfc/rfc9052) / [RFC 9053](https://www.rfc-editor.org/rfc/rfc9053) - CBOR Object Signing and Encryption (COSE)
- [RFC 8392](https://www.rfc-editor.org/rfc/rfc8392) - CBOR Web Token (CWT)
- [RFC 8747](https://www.rfc-editor.org/rfc/rfc8747) - Proof-of-Possession Key Semantics for CBOR Web Tokens (CWTs)
- [RFC 9528](https://www.rfc-editor.org/rfc/rfc9528) - Ephemeral Diffie-Hellman Over COSE (EDHOC) (`kccs` ヘッダ)
- [RFC 9679](https://www.rfc-editor.org/rfc/rfc9679) - CBOR Object Signing and Encryption (COSE) Key Thumbprint
- [RFC 7515](https://www.rfc-editor.org/rfc/rfc7515) - JSON Web Signature (JWS)
- [RFC 7517](https://www.rfc-editor.org/rfc/rfc7517) - JSON Web Key (JWK)
- [RFC 7638](https://www.rfc-editor.org/rfc/rfc7638) - JSON Web Key (JWK) Thumbprint
- [RFC 9449](https://www.rfc-editor.org/rfc/rfc9449) - OAuth 2.0 Demonstrating Proof of Possession (DPoP)
- [RFC 9596](https://www.rfc-editor.org/rfc/rfc9596) - CBOR Object Signing and Encryption (COSE) "typ" (type) Header Parameter

一次資料は [`../refs/cbor/`](../refs/cbor) と [`../refs/moq/`](../refs/moq) にあります。

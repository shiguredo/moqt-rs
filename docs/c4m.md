# C4M (Common Access Token for MoQ)

MOQT の認可トークンを扱う `c4m` モジュールの解説です。
[draft-ietf-moq-c4m-01](https://datatracker.ietf.org/doc/html/draft-ietf-moq-c4m-01) が定義するトークンと、その基盤となる CBOR、COSE、CWT、CAT を実装しています。
モジュール構成とアーキテクチャは [`IMPLEMENTATION.md`](IMPLEMENTATION.md)、他の仕様の実装状況は [`moqt.md`](moqt.md) / [`msf.md`](msf.md) / [`loc.md`](loc.md)、利用者向けの API 一覧は [`../skills/shiguredo-moqt/SKILL.md`](../skills/shiguredo-moqt/SKILL.md) を参照してください。

## 概要

- トークンは CAT (Common Access Token) です。CAT は CWT (CBOR Web Token) と COSE を使って署名または MAC されます
- MOQT へは Auth Token Type `0x01` (CAT) の Token Value として載せます (§7.1.1)
- `moqt` クレーム (claim key 327) でアクション、名前空間、トラック名の範囲を指定します
- DPoP を使うとトークンをクライアントの鍵に束縛できます (`cnf` の `jkt` と `catdpop` の設定)
- Sans-I/O です。時計は検証 API の引数で渡し、`moqt-reval` の再検証の実行は利用側が行います
- 既定ビルドは `no_std` です。暗号処理は `CoseCrypto` trait に分離し、aws-lc-rs を使う実装を optional feature `aws-lc-rs` で提供します

## モジュール構成

| モジュール | 役割 |
| --- | --- |
| `c4m` | `MoqtAction`、`MoqtScope`、`MoqtClaim`、`Match`、`NamespaceMatch`、`CatDpop` |
| `c4m::cbor` | CBOR (RFC 8949) の encode / decode |
| `c4m::cose` | COSE (RFC 9052) の構造、ヘッダ、アルゴリズム |
| `c4m::crypto` | `CoseCrypto` trait、`CoseKey`、エラー型 (aws-lc-rs 実装は feature 分岐) |
| `c4m::cat` | CAT のクレーム、トークン、発行ビルダー、検証 |
| `c4m::jwk` | JWK (RFC 7517) と JWK サムプリント (RFC 7638) |
| `c4m::jwt` | JWS compact (RFC 7515) の JWT |
| `c4m::dpop` | DPoP proof の検証と発行 (draft-nandakumar-moq-generic-dpop-proof-00) |

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
  - 直列化は compact 形式 (draft-ietf-moq-c4m-01 付録 A のテストベクタ) と COSE 形式 (CWT タグ + COSE_Sign1 / COSE_Mac0)、および URL 埋め込み用の標準 Base64
  - 発行 (署名) と検証。アルゴリズムは HMAC 256/384/512、ES256/384/512、EdDSA (Ed25519)
  - 付録 A のテストベクタ (CBOR エンコード / トークン構造 / DPoP バインディング / 認可マッチング / 検証) をテストで固定
- `moqt` クレーム
  - `moqt` (claim key 327) のアクション / 名前空間 / トラックの exact / prefix / suffix マッチと `nil` の末尾固定
  - `moqt-reval` (claim key 328)
  - `CatClaims::authorize` による認可判定
- DPoP
  - `cnf` の JWK サムプリント (`jkt`) と `catdpop` (ウィンドウ / jti)
  - JWT (JWS compact) の DPoP proof の検証 (署名 / 鍵バインディング / 鮮度 / `ath` / Authorization Context / リプレイ)
  - DPoP proof の発行 (`DpopProofBuilder`)
- 暗号
  - `CoseCrypto` trait に署名 / 検証 / ハッシュを分離し、既定ビルドは暗号実装を一切リンクしない
  - `aws-lc-rs` feature で aws-lc-rs を使う実装を提供する (no_std ターゲットでは使えない)

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
- マップの重複キーは拒否します。RFC 8949 §5.6.1 の等価規則に従い、`-0.0` と `0.0` は同一、NaN はすべて同一として扱います
- テキスト文字列は UTF-8 であることを検証します。indefinite 長はチャンクごとに検証します (§3.2.3)
- 予約された additional information (28〜30)、予約された単純値 (24〜31)、末尾の余分なバイトは拒否します
- 複数のデータ項目を読む場合は `decode_partial` を使います。消費したバイト数を返します

### テスト

付録 A.2 の 7 ベクタ (decode と再エンコードの一致)、境界値 (整数の各幅、f16 / f32 / f64、indefinite、深度、重複キー、UTF-8)、PBT のラウンドトリップ、fuzz ターゲットで固定しています。

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

### ヘッダ

| ラベル | 名前 | 扱い |
| --- | --- | --- |
| 1 | `alg` | protected ヘッダに必須。unprotected にしかない場合は拒否 |
| 2 | `crit` | protected ヘッダのみ。配列は 1 要素以上。ラベルは同じ protected ヘッダに実在し、理解できる必要がある |
| 3 | `content type` | protected と unprotected のどちらでも可 |
| 4 | `kid` | バイト文字列とテキスト文字列の両方を受理 |
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
- draft-ietf-moq-c4m-01 付録 A のベクタは HMAC-SHA256 の識別子に `-4` を使います。IANA では `-4` は A192KW のため、**検証では `-4` と `5` の両方を HMAC-SHA256 として受理**し、発行は `5` を使います (`C4M_DRAFT_HMAC_SHA256_ALGORITHM_ID`)
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
use shiguredo_moqt::c4m::cat::{ClaimValidationOptions, ClaimValidationError};

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

| 形式 | 表現 | 署名対象 |
| --- | --- | --- |
| compact | `base64url(protected).base64url(claims).base64url(signature)` | ASCII の `protected.claims` |
| COSE | CWT タグ + COSE_Sign1 / COSE_Mac0 の CBOR | `Sig_structure` / `MAC_structure` |
| URL 埋め込み | 標準 Base64 (RFC 4648 §4) のテキスト (§2 / §4) | 形式による |

`CatToken::decode` は compact 形式、COSE 形式の CBOR、base64url または標準 Base64 で包んだテキストの順に自動判別します。
パディングの有無はどちらも受理します。

compact 形式は draft-ietf-moq-c4m-01 付録 A のテストベクタの形式です。COSE 形式は RFC 8392 / RFC 9052 の形式です。

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

これらの値の意味論 (`catu` の URI マッチ評価など) は実装していません。キーと値の保持だけを行います。

### `moqt` クレーム (claim key 327)

`moqt` クレームはアクションスコープの配列です。

```text
moqt-value = [ + moqt-scope ]
moqt-scope = [ moqt-actions, ? [ + moqt-ns-match ], ? moqt-track-match ]
moqt-actions = [ + moqt-action ]
moqt-ns-match = bin-match / nil
moqt-track-match = bin-match
bin-match = bstr / [ match-type, match-value ]
match-type = 1 (prefix) / 2 (suffix)
```

| アクション | key |
| --- | --- |
| CLIENT_SETUP | 0 |
| SERVER_SETUP | 1 |
| PUBLISH_NAMESPACE | 2 |
| SUBSCRIBE_NAMESPACE | 3 |
| SUBSCRIBE | 4 |
| REQUEST_UPDATE | 5 |
| PUBLISH | 6 |
| FETCH | 7 |
| TRACK_STATUS | 8 |

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

### `moqt-reval` クレーム (claim key 328)

再検証の間隔 (秒) です。

- `0` の場合は再検証しません。クレームが無い場合も同じです
- 受信者が再検証できない場合、および再検証間隔が自身の能力を下回る場合は、そのトークンを拒否する MUST があります。本ライブラリは Sans-I/O のため再検証を実行せず、拒否の判断は利用側が行います
- クレームが composition claims の中にあるトークンは well-formed ではありません (本ライブラリは複合クレームを評価しないため、`moqt` が読めず全アクションがブロックされます)

### DPoP バインディング (`cnf` と `catdpop`)

- `cnf` の `jkt` は JWK サムプリント (32 バイト) です。IANA 登録の confirmation key 323 を優先し、無い場合はドラフトのベクタが使う 3 (`kid`) と比較します
- `catdpop` (claim key 321) は label 0 が受理ウィンドウ (秒)、label 1 が jti によるリプレイ保護の有無です

### 発行と検証

```rust
use shiguredo_moqt::c4m::cat::{CAT_CONTENT_TYPE, CatTokenBuilder, VerifyOptions};
use shiguredo_moqt::c4m::crypto::CoseKey;
use shiguredo_moqt::c4m::crypto::aws_lc_rs::AwsLcRsCrypto;

let crypto = AwsLcRsCrypto::new();
let key = CoseKey::symmetric(shared_secret);

// 発行 (compact 形式は String、COSE 形式は Vec<u8> を返します)
let token_text = CatTokenBuilder::new()
    .issuer("https://auth.example.com")
    .audience("https://relay.example.com")
    .issued_at(unix_now)
    .expiration(unix_now + 3600.0)
    .moqt(moqt_claim)
    .moqt_reval(300.0)
    .jwk_thumbprint(jkt)
    .catdpop(300.0, true)
    .build_compact(&crypto, &key)?;

// 検証
let token = CatToken::decode(token_text.as_bytes())?;
token.verify_with(
    &crypto,
    &key,
    &VerifyOptions {
        expected_algorithm: Some(Algorithm::HmacSha256),
        expected_type: Some(CAT_CONTENT_TYPE),
    },
)?;
```

- `CatTokenBuilder` はクレームと `moqt` スコープを組み立て、`build_compact` / `build_cose` で署名します
- 署名アルゴリズムは鍵の種別から自動選択します (対称鍵は HMAC-SHA256、EC2 は ES256 / ES384 / ES512、OKP は EdDSA)。`algorithm` で明示もできます
- `typ` は既定で `CAT` です。`VerifyOptions::expected_type` に `CAT_CONTENT_TYPE` を渡すと `typ` を検証できます
- COSE 形式の発行は CWT タグと COSE タグを付与します。タグの付与は `CoseEncodingOptions` で制御できます
- 発行するクレームの数値が有限でない場合、および型付きキーを `raw` に置いた場合はエラーを返します

### MOQT の Auth Token Type

`MOQT_AUTH_TOKEN_TYPE_CAT` が `0x01` です。Token Value からデコードする場合は `CatToken::decode_moqt_auth_token` を使います。

```rust
use shiguredo_moqt::c4m::cat::{CatToken, MOQT_AUTH_TOKEN_TYPE_CAT};

let token = CatToken::decode_moqt_auth_token(MOQT_AUTH_TOKEN_TYPE_CAT, token_value)?;
```

### 仕様側の未解決事項

実装は次の仕様の揺れを吸収しています。確定したら追従が必要です。

- トークンの直列化: §7.1.1 は CBOR の CWT と読めますが、付録 A のテストベクタは compact 形式です。本ライブラリは両方を扱います
- HMAC-SHA256 の識別子: 付録 A のベクタは `-4`、IANA は `5` です。検証は両方、発行は `5` です
- `cnf` の `jkt`: 付録 A のベクタは confirmation key 3、IANA は 323 です。検証は 323 を優先します

### 未対応

- draft-lemmons-cose-composite-claims の複合論理クレーム (`/or/` など)。この形式のトークンは `moqt` が読めないため、すべてのアクションがブロックされます。複数スコープを並べる形式には対応しています
- CTA-5007-B の HTTP 向けクレームの意味論 (`catu` のマッチ評価、`catr` の更新など)
- CWT 形式の DPoP proof (`dpop-proof+cwt`)。actx の claim label が TBD のためです
- RSA (PS256 / RS256 など)

## DPoP (draft-nandakumar-moq-generic-dpop-proof-00)

DPoP はトークンをクライアントの秘密鍵に束縛し、盗まれたトークンの再利用を防ぎます。
C4M は JWT 形式の proof を使います (`typ` は `dpop-proof+jwt`)。

### 検証

`DpopProof::verify_against_cat_token` が次の項目をこの順で検証します。

1. proof の署名 (埋め込み JWK で検証。JWK に秘密鍵がある proof は拒否)
2. `ath` (トークンの `raw_token()` と照合。アクセストークンを伴う proof は `ath` が必須)
3. `cnf` の `jkt` と JWK サムプリントの一致
4. Authorization Context (`actx.type` は `moqt`、action、`tns` / `tn`、`resource` の整合)
5. `iat` の鮮度 (`catdpop` のウィンドウ)
6. jti のリプレイ (`catdpop` が jti を要求する場合は `DpopReplayCache` が必須)

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

アクセストークンを伴わない proof (token endpoint など) は `verify_against_token` に `access_token: None` を渡します。
サーバーが `nonce` を発行している場合の照合はアプリ側で `proof.claims().nonce` を確認してください (`verify_against_cat_token` は検証しません)。

### 発行

クライアント側は `DpopProofBuilder` で proof を発行します。
`AuthorizationContext` の `tns` / `tn` は `name::serialize_namespace` / `name::serialize_track_name` の正規シリアライズを使います。

```rust
use shiguredo_moqt::c4m::dpop::{AuthorizationContext, DpopProofBuilder, MOQT_AUTHORIZATION_CONTEXT_TYPE};
use shiguredo_moqt::c4m::crypto::DigestAlgorithm;
use shiguredo_moqt::name::{serialize_namespace, serialize_track_name};

let actx = AuthorizationContext {
    context_type: String::from(MOQT_AUTHORIZATION_CONTEXT_TYPE),
    action: String::from(MoqtAction::Publish.authorization_context()),
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

- 署名は非対称アルゴリズムだけです。HMAC は DPoP では使えません
- 埋め込む JWK と署名鍵が同じ公開鍵でない場合は `KeyMismatch` で失敗します
- `AuthorizationContext` の型付きフィールドは `type`、`action`、`tns`、`tn`、`resource` の 5 つです。`raw` にしか無い拡張フィールドは発行時に出力されません

## 対応仕様

- [draft-ietf-moq-c4m-01](https://datatracker.ietf.org/doc/html/draft-ietf-moq-c4m-01) - Authorization scheme for MOQT using Common Access Tokens
- [draft-nandakumar-moq-generic-dpop-proof-00](https://datatracker.ietf.org/doc/draft-nandakumar-moq-generic-dpop-proof/) - Application-Agnostic Demonstrating Proof-of-Possession
- [RFC 8949](https://www.rfc-editor.org/rfc/rfc8949) - Concise Binary Object Representation (CBOR)
- [RFC 9052](https://www.rfc-editor.org/rfc/rfc9052) / [RFC 9053](https://www.rfc-editor.org/rfc/rfc9053) - CBOR Object Signing and Encryption (COSE)
- [RFC 8392](https://www.rfc-editor.org/rfc/rfc8392) - CBOR Web Token (CWT)
- [RFC 8747](https://www.rfc-editor.org/rfc/rfc8747) - CBOR Web Token (CWT) Claims in COSE Headers
- [RFC 7515](https://www.rfc-editor.org/rfc/rfc7515) - JSON Web Signature (JWS)
- [RFC 7517](https://www.rfc-editor.org/rfc/rfc7517) - JSON Web Key (JWK)
- [RFC 7638](https://www.rfc-editor.org/rfc/rfc7638) - JSON Web Key (JWK) Thumbprint
- [RFC 9449](https://www.rfc-editor.org/rfc/rfc9449) - OAuth 2.0 Demonstrating Proof of Possession (DPoP)
- [RFC 9596](https://www.rfc-editor.org/rfc/rfc9596) - CBOR Object Signing and Encryption (COSE) "typ" (type) Header Parameter

一次資料は [`../refs/cbor/`](../refs/cbor) と [`../refs/moq/`](../refs/moq) にあります。

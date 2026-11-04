//! Low Overhead Media Container (LOC) プロパティ
//!
//! draft-ietf-moq-loc-04 に基づく実装。
//! 本仕様は draft 由来であり、将来の改訂で変更される可能性がある。
//!
//! LOC Properties は Public と Private に分類される (draft-ietf-moq-loc-04 §2.2):
//!   - LOC Public Properties: MOQ Object Properties に含まれ、リレーから参照可能
//!   - LOC Private Properties: MOQ Object Payload に含まれ、E2E 暗号化で保護可能
//!
//! エンコード形式は draft-ietf-moq-transport-23 §11.1.2 (Object Properties) および draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) に従う。
//!
//! 本モジュールは Public/Private の区別なくプロパティの encode/decode を提供する。
//! Public/Private の配置はアプリケーション層の責務である。
//!
//! # 多重可否
//!
//! draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は、Property の定義が
//! 複数を許す場合を除いて Track / Object に 1 インスタンスだけ載せてよいと定め、許される数より
//! 多く受信したエンドポイントに Track を malformed とすることを MUST とする (§12.1
//! (Malformed Tracks) 条件 9)。LOC Properties の各定義 (draft-ietf-moq-loc-05 §2.3) にも
//! 複数を許す規則は無いため、この実装が意味を理解する既知 ID は単一に限られる。未知 ID は
//! §8.4 の "MUST allow multiple instances of the Property" と §16.8 (Properties) の
//! "Endpoints MUST ignore unknown Property types" に従って複数インスタンスを保持する。多重
//! 可否の表は `crate::property_scope` に集約する。
//!
//! # IMMUTABLE_PROPERTIES の内側探索
//!
//! draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) は、relay から見える不変の LOC
//! Properties を IMMUTABLE_PROPERTIES (0x0B) として符号化することを SHOULD とする。
//! draft-ietf-moq-transport-23 §10.7 (Immutable Properties) は、値の取得時に mutable
//! プロパティ列と IMMUTABLE_PROPERTIES の内側の両方を探索することを MUST とする。
//! [`LocProperties::find_varint`] / [`LocProperties::find_bytes`] と既知プロパティの
//! accessor は最上位リストを優先し、無ければ IMMUTABLE_PROPERTIES の内側を探索する。
//! [`LocProperties::iter`] は最上位リストだけを返す。

use crate::track_properties::PROP_IMMUTABLE_PROPERTIES;
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// 単一に限られる既知プロパティが複数現れたときの説明
///
/// 受信時の `MalformedTrack` とローカル入力検証時の `ProtocolViolation` が同じ理由を
/// 指すように 1 箇所に置く (どちらも `MessageError` の `&'static str` である)。
const DUPLICATE_LOC_PROPERTY: &str = "duplicate LOC property ID";

/// Audio Level の最大値 (下位 8 bit = 0-255)
///
/// draft-ietf-moq-loc-04 §2.3.3.2: `least significant 8 bits of a vi64`
/// draft の `Value: vi64 (1-2 bytes to encode values 0x00-0xFF)` と整合する。
const AUDIO_LEVEL_MAX: u64 = 255;

/// 既知プロパティの値域を検証する
///
/// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure):
/// 受信側が既知 Type の Value が定義された serialization と一致しない場合、
/// MALFORMED_KEY_VALUE でセッションを閉じなければならない (MUST)。
///
/// 送信側も不正な値を送出しないよう同じ検証を行う。
fn validate_known_property(prop_id: u64, value: u64) -> Result<(), MessageError> {
    match prop_id {
        PROP_AUDIO_LEVEL if value > AUDIO_LEVEL_MAX => Err(MessageError::MalformedKeyValue(
            "Audio Level value exceeds 8-bit range (draft-ietf-moq-loc-04 §2.3.3.2)",
        )),
        _ => Ok(()),
    }
}

/// 既知プロパティのバイト列長を検証する
///
/// draft-ietf-moq-loc-04 §2.3.2.2: Video Frame Marking の `Length: Varies (1-4 bytes)`。
/// draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure):
/// 受信側が既知 Type の Length が定義された serialization と一致しない場合、
/// MALFORMED_KEY_VALUE でセッションを閉じなければならない (MUST)。
///
/// `len` は宣言長 (encode 時は `Bytes` の実長、decode 時は length フィールドの値) であり、
/// 汎用長さ検査 (65535 超過) の直後、decode では切り詰め検査より前に呼ぶ。
fn validate_known_property_len(prop_id: u64, len: u64) -> Result<(), MessageError> {
    match prop_id {
        PROP_VIDEO_FRAME_MARKING if !(1..=4).contains(&len) => {
            Err(MessageError::MalformedKeyValue(
                "Video Frame Marking length must be 1-4 bytes (draft-ietf-moq-loc-04 §2.3.2.2)",
            ))
        }
        _ => Ok(()),
    }
}

/// Timestamp プロパティ ID (偶数 → varint 値)
///
/// draft-ietf-moq-loc-04 §2.3.1.1 (ID=0x10, Value: vi64 1-9 bytes)
/// IANA 登録済み (§6.1: MOQ Properties Registry, Type=0x10, Scope=Object)
///
/// エンコードされたメディアフレームのタイムスタンプを vi64 で表現する。
/// Timescale プロパティ (§2.3.1.2) が存在しない場合は
/// Unix エポック以降のマイクロ秒単位の壁時計時刻として解釈される。
/// Timescale プロパティが存在する場合は、Timescale で定義された単位のメディア時刻として解釈される。
///
/// 値域は u64 全域 (vi64 1-9 bytes) のため上限検証は不要。
pub const PROP_TIMESTAMP: u64 = 0x10;

/// Timescale プロパティ ID (偶数 → varint 値)
///
/// draft-ietf-moq-loc-04 §2.3.1.2 (ID=0x08, Value: vi64 1-9 bytes)
/// IANA 登録済み (§6.1: MOQ Properties Registry, Type=0x08, Scope=Track, Object)
///
/// Timestamp プロパティの単位を「1 秒あたりの Timestamp ユニット数」で定義する。
/// 一般的な値:
///   - 1000000: マイクロ秒 (デフォルトと同等)
///   - 48000: 48kHz オーディオのサンプルレート
///   - 90000: ビデオの 90kHz クロックレート
///
/// このプロパティが存在する場合、Timestamp はメディア時刻を表す。
/// エポック (基準点) はアプリケーション定義である。
/// このプロパティが存在しない場合、Timestamp は Unix エポック以降のマイクロ秒にデフォルトする。
pub const PROP_TIMESCALE: u64 = 0x08;

/// Video Frame Marking プロパティ ID (奇数 → バイト列)
///
/// draft-ietf-moq-loc-04 §2.3.2.2 (ID=0x09, Length: Varies (1-4 bytes))
/// IANA 登録済み (§6.1: MOQ Properties Registry, Type=0x09, Scope=Object)
///
/// RFC 9626 で定義されるビデオフレームのフラグ。
/// 独立フレーム、破棄可能フレーム、ベースレイヤ同期ポイント、
/// テンポラル/スペーシャルレイヤ識別子を長さプレフィックス付きバイト列としてエンコードする。
pub const PROP_VIDEO_FRAME_MARKING: u64 = 0x09;

/// Audio Level プロパティ ID (偶数 → varint 値)
///
/// draft-ietf-moq-loc-04 §2.3.3.2 (ID=0x0C, Value: vi64 1-2 bytes, 0x00-0xFF)
/// IANA 登録済み (§6.1: MOQ Properties Registry, Type=0x0C, Scope=Object)
///
/// RFC 6464 で定義される音声レベルと音声アクティビティ。
/// vi64 の下位 8 bit にエンコードする。
pub const PROP_AUDIO_LEVEL: u64 = 0x0C;

/// Video Config プロパティ ID (奇数 → バイト列)
///
/// draft-ietf-moq-loc-04 §2.3.2.1 (ID=0x0D, Length: Varies)
/// IANA 登録済み (§6.1: MOQ Properties Registry, Type=0x0D, Scope=Track, Object)
///
/// ビデオコーデック設定 ("extradata")。
/// 対応するコーデック仕様で定義され、WebCodecs の
/// VideoDecoderConfig description プロパティにマップされる。
pub const PROP_VIDEO_CONFIG: u64 = 0x0D;

/// Audio Config プロパティ ID (奇数 → バイト列)
///
/// draft-ietf-moq-loc-04 §2.3.3.1 (ID=0x0F, Length: Varies)
/// IANA 登録済み (§6.1: MOQ Properties Registry, Type=0x0F, Scope=Track, Object)
///
/// 音声コーデックの configuration bytes。
/// WebCodecs の AudioDecoderConfig description プロパティに対応する。
pub const PROP_AUDIO_CONFIG: u64 = 0x0F;

/// LOC プロパティの値
///
/// draft-ietf-moq-loc-04 §2.3
/// プロパティ ID の偶奇で値の型が決まる:
///   - 偶数 ID: varint 値 (VarInt)
///   - 奇数 ID: 長さプレフィックス付きバイト列 (Bytes)
#[derive(Debug, Clone, PartialEq)]
pub enum LocPropertyValue {
    /// 偶数 ID のプロパティ値 (varint)
    VarInt(u64),
    /// 奇数 ID のプロパティ値 (バイト列, 最大 65535 バイト)
    Bytes(Vec<u8>),
}

/// LOC プロパティエントリ
///
/// draft-ietf-moq-loc-04 §2.3
/// 1 つのプロパティは ID と値のペアで構成される。
#[derive(Debug, Clone, PartialEq)]
pub struct LocProperty {
    /// プロパティ ID (draft-ietf-moq-loc-04 §2.3)
    pub prop_id: u64,
    /// プロパティ値 (draft-ietf-moq-loc-04 §2.3)
    pub value: LocPropertyValue,
}

/// LOC プロパティのコレクション
///
/// draft-ietf-moq-loc-04 §2.3
/// `encode()` / `decode()` は Properties Length を含む完全なブロックを扱う。
///
/// ワイヤーフォーマット:
///   Properties Length (varint) | Key-Value-Pairs...
///
/// エンコード時は prop_id の昇順にソートし、delta encoding で ID を圧縮する。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LocProperties(Vec<LocProperty>);

impl LocProperties {
    /// 空のコレクションを作成する
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// プロパティを追加する
    pub fn push(&mut self, prop: LocProperty) {
        self.0.push(prop);
    }

    /// 最上位 (mutable) リストが空かどうかを返す
    ///
    /// IMMUTABLE_PROPERTIES (0x0B) の内側は対象にしない ([`LocProperties::len`] と同じ範囲)。
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 最上位 (mutable) リストのプロパティ数を返す
    ///
    /// IMMUTABLE_PROPERTIES (0x0B) の内側のプロパティは数えない。
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 最上位 (mutable) リストのプロパティエントリのイテレータを返す
    ///
    /// IMMUTABLE_PROPERTIES (0x0B) の内側は列挙しない。内側の値が必要な場合は
    /// [`LocProperties::find_varint`] / [`LocProperties::find_bytes`] または既知プロパティの
    /// accessor を使う (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の
    /// "MUST search both")。
    pub fn iter(&self) -> impl Iterator<Item = &LocProperty> {
        self.0.iter()
    }

    /// LOC プロパティブロック全体をエンコードする
    ///
    /// prop_id の昇順にソートして delta encoding でエンコードする。
    /// 空のコレクションは Properties Length = 0 としてエンコードする。
    /// 未知 ID は複数インスタンスを保持したまま書き出す (draft-ietf-moq-transport-23 §8.4
    /// (Track and Object Properties) の "MUST allow multiple instances of the Property")。
    /// 同一 ID の並び順はソートが安定であるため入力順を保つ。
    ///
    /// IMMUTABLE_PROPERTIES (0x0B) の値はバイト列としてそのまま書き出し、内側の KVP 列は
    /// 解釈も検証もしない (値の取得時に [`LocProperties::find_varint`] /
    /// [`LocProperties::find_bytes`] が検証する)。
    ///
    /// # Errors
    ///
    /// - 偶数 ID に `Bytes` 値: `ProtocolViolation`
    /// - 奇数 ID に `VarInt` 値: `ProtocolViolation`
    /// - 単一に限られる既知 ID の重複: `ProtocolViolation`
    ///   (ローカル入力の検証であり、受信 Track の malformed 判定とは別)
    /// - バイト列が 65535 バイトを超える: `PayloadTooLong`
    /// - 既知プロパティの値域違反 (Audio Level が 255 超過): `MalformedKeyValue`
    /// - Video Frame Marking の長さが 1-4 バイトでない: `MalformedKeyValue`
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        // draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header):
        // "Objects with no properties or non-Normal status set Properties Length to 0."
        if self.0.is_empty() {
            let mut buf = Vec::new();
            varint::encode(0, &mut buf);
            return Ok(buf);
        }

        // prop_id の昇順にソート (delta encoding のため差分が常に正)
        let mut sorted = self.0.clone();
        sorted.sort_by_key(|p| p.prop_id);

        // 単一に限られる既知プロパティの重複を検出する (draft-ietf-moq-transport-23 §8.4
        // (Track and Object Properties) / §12.1 (Malformed Tracks) 条件 9)。未知 ID は
        // 複数インスタンスを許す。encode はローカル入力の検証であるため ProtocolViolation で
        // 拒否する。
        if crate::property_scope::has_duplicate_single_property(sorted.iter().map(|p| p.prop_id)) {
            return Err(MessageError::ProtocolViolation(DUPLICATE_LOC_PROPERTY));
        }

        let mut inner = Vec::new();
        let mut prev_id: u64 = 0;

        for prop in &sorted {
            crate::kvp::encode_delta_key(prev_id, prop.prop_id, &mut inner);
            prev_id = prop.prop_id;

            match &prop.value {
                LocPropertyValue::VarInt(v) => {
                    if prop.prop_id % 2 != 0 {
                        return Err(MessageError::ProtocolViolation(
                            "odd property ID requires Bytes value",
                        ));
                    }
                    validate_known_property(prop.prop_id, *v)?;
                    varint::encode(*v, &mut inner);
                }
                LocPropertyValue::Bytes(b) => {
                    if prop.prop_id % 2 == 0 {
                        return Err(MessageError::ProtocolViolation(
                            "even property ID requires VarInt value",
                        ));
                    }
                    if b.len() > 65535 {
                        return Err(MessageError::PayloadTooLong);
                    }
                    validate_known_property_len(prop.prop_id, b.len() as u64)?;
                    varint::encode(b.len() as u64, &mut inner);
                    inner.extend_from_slice(b);
                }
            }
        }

        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        Ok(buf)
    }

    /// バッファ先頭から LOC プロパティブロックをデコードし `(properties, 消費バイト数)` を返す
    ///
    /// フォーマット: `Properties Length (varint) | Key-Value-Pairs...`
    ///
    /// 未知 ID の複数インスタンスは正当として保持する (draft-ietf-moq-transport-23 §8.4
    /// (Track and Object Properties) の "MUST allow multiple instances of the Property")。
    ///
    /// IMMUTABLE_PROPERTIES (0x0B) の内側は構造として保持するだけで、この関数では検証しない。
    /// 内側の検証 (重複 ID・既知プロパティの値域と長さ) は [`LocProperties::find_varint`] /
    /// [`LocProperties::find_bytes`] の探索時に、この関数と共通の KVP パーサーで行う。内側が
    /// 破損している場合は探索結果を `None` とし、この関数の戻り値とエラー分類は変えない。
    ///
    /// # Errors
    ///
    /// - delta-key のオーバーフロー: `DeltaEncodingOverflow`
    ///   (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の
    ///   DELTA_ENCODING_OVERFLOW)
    /// - 単一に限られる既知 ID の重複: `MalformedTrack`
    ///   (draft-ietf-moq-transport-23 §8.4 / §12.1 (Malformed Tracks) 条件 9。呼び出し側は
    ///   セッション終了ではなく、同じ Track の購読・fetch の取消へ渡す)。本関数が検出するのは
    ///   最上位 (mutable) リスト内の重複だけであり、最上位と IMMUTABLE_PROPERTIES の内側に
    ///   同じ既知 ID が現れる場合は検出しない (§10.7 は複数の値を許すプロパティに限り両方への
    ///   出現を許す。単一値プロパティが両方に現れると §8.4 / §12.1 条件 9 の malformed になるが、
    ///   その判定は本モジュールの範囲外)
    /// - 奇数 ID の宣言長が 65535 バイトを超える: `FieldLengthExceeded`
    /// - Audio Level が 255 を超える: `MalformedKeyValue`
    /// - Video Frame Marking の宣言長が 1-4 バイトでない: `MalformedKeyValue`
    ///   (宣言長の検証は切り詰め検査 `UnexpectedEof` より先に行う)
    /// - バッファが不足する: `UnexpectedEof`
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (prop_len, n) = varint::decode(&buf[pos..])?;
        pos += n;

        // draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header):
        // "Objects with no properties or non-Normal status set Properties Length to 0."
        if prop_len == 0 {
            return Ok((Self(Vec::new()), pos));
        }

        let prop_len = varint::checked_len(prop_len, buf[pos..].len())?;

        let inner = &buf[pos..pos + prop_len];
        pos += prop_len;

        // 最上位リストの走査は IMMUTABLE_PROPERTIES の内側探索と共通の walk_kv_pairs に置く。
        // この経路では 0x0B を通常の奇数 ID プロパティとして保持するため、入れ子の 0x0B も
        // 従来どおり受理する (decode の戻り値とエラー分類を変えない)。
        let properties = walk_kv_pairs(inner, true)?
            .into_iter()
            .map(LocPropertyRef::to_owned_property)
            .collect();

        Ok((Self(properties), pos))
    }

    // ─── 既知プロパティアクセサ ──────────────────────────────────────
    //
    // 各 accessor は最上位リストを優先し、無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も
    // 探索する (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の "MUST search
    // both")。探索規則の詳細は find_varint / find_bytes を参照。

    /// Timestamp (ID=0x10): エンコードされたメディアフレームのタイムスタンプ
    ///
    /// draft-ietf-moq-loc-04 §2.3.1.1
    /// Timescale プロパティが存在しない場合は Unix エポック以降のマイクロ秒。
    /// Timescale プロパティが存在する場合は Timescale で定義された単位のメディア時刻。
    /// 最上位リストに無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    pub fn timestamp(&self) -> Option<u64> {
        self.find_varint(PROP_TIMESTAMP)
    }

    /// Timescale (ID=0x08): Timestamp の単位を定義する (1 秒あたりのユニット数)
    ///
    /// draft-ietf-moq-loc-04 §2.3.1.2 (Value: vi64 1-9 bytes = u64 全域)
    /// 値域は u64 全域のため、Timestamp と同様に上限検証は不要。
    /// 一般的な値: 1000000 (マイクロ秒), 48000 (48kHz オーディオ), 90000 (90kHz ビデオ)
    /// このプロパティが存在しない場合、Timestamp は Unix エポック以降のマイクロ秒にデフォルトする。
    /// 最上位リストに無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    pub fn timescale(&self) -> Option<u64> {
        self.find_varint(PROP_TIMESCALE)
    }

    /// Video Frame Marking (ID=0x09): RFC 9626 に基づくビデオフレームフラグ
    ///
    /// draft-ietf-moq-loc-04 §2.3.2.2 (奇数 ID のバイト列、長さ 1-4 bytes)
    /// フラグの内容は解釈せず、バイト列としてそのまま返す。
    /// 最上位リストに無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    pub fn video_frame_marking(&self) -> Option<&[u8]> {
        self.find_bytes(PROP_VIDEO_FRAME_MARKING)
    }

    /// Audio Level (ID=0x0C): RFC 6464 に基づく音声レベルと音声アクティビティ (下位 8 bit)
    ///
    /// draft-ietf-moq-loc-04 §2.3.3.2 (偶数 ID の varint 値、0x00-0xFF)
    /// 最上位リストに無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    pub fn audio_level(&self) -> Option<u64> {
        self.find_varint(PROP_AUDIO_LEVEL)
    }

    /// Video Config (ID=0x0D): ビデオコーデック設定 (extradata)
    ///
    /// draft-ietf-moq-loc-04 §2.3.2.1
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    /// 最上位リストに無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    pub fn video_config(&self) -> Option<&[u8]> {
        self.find_bytes(PROP_VIDEO_CONFIG)
    }

    /// Audio Config (ID=0x0F): 音声コーデックの configuration bytes
    ///
    /// draft-ietf-moq-loc-04 §2.3.3.1
    /// WebCodecs の AudioDecoderConfig description プロパティに対応する。
    /// LOC 層はコーデックの内容を解釈せず、opaque bytes として返す。
    /// 最上位リストに無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    pub fn audio_config(&self) -> Option<&[u8]> {
        self.find_bytes(PROP_AUDIO_CONFIG)
    }

    /// 指定 ID の varint 値を取得する
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の "When looking for the
    /// value of a property, processors MUST search both the mutable properties and the
    /// contents of Immutable Properties." に従い、最上位リストを先に探索し、無ければ
    /// IMMUTABLE_PROPERTIES (0x0B) の内側を delta-key KVP 列として探索する。
    ///
    /// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) は、relay から見える不変の LOC
    /// Properties を IMMUTABLE_PROPERTIES として符号化することを SHOULD とする。どの
    /// プロパティを immutable にするかは publisher の判断であり、本実装は値の取得時に
    /// transport 側の MUST に従って内側も探索する。
    ///
    /// ID と値形式 (偶数 ID は varint) の両方が一致する最初のインスタンスを返す。最上位と
    /// 内側の両方に同じ ID がある場合は最上位を優先する (既存の `ObjectProperties` /
    /// `TrackProperties` の getter と同じ規則)。
    ///
    /// 最上位リストの多重検証は行わず、最初の一致を返す。IMMUTABLE_PROPERTIES の内側を探索
    /// する場合は内側の KVP 列の検証 (値域・長さ・既知 ID の重複を含む) を行い、内側が破損
    /// している場合・入れ子の IMMUTABLE_PROPERTIES (0x0B) を検出した場合・内側に単一に
    /// 限られる既知 ID の重複がある場合は `None` を返す。内側は 1 回の走査で全体を検証する
    /// ため、要求 ID の一致より後ろに構造エラーがある場合も `None` になる。
    ///
    /// 内側だけにある LOC 固有の値域・長さ違反 (Audio Level の 8 bit 超過や Video Frame
    /// Marking の長さ違反など) も探索時に検出して `None` に写す。この扱いは既存の
    /// `ObjectProperties` / `TrackProperties` の getter と同じであり、LOC 固有規則を知る
    /// 層 (本モジュールまたはその呼び出し側) で `MalformedKeyValue` を返すかは別課題である。
    ///
    /// この探索規則は draft 由来であり、将来の draft 改版で変わる可能性がある。
    pub fn find_varint(&self, prop_id: u64) -> Option<u64> {
        match self.find_property(prop_id, FindValue::VarInt)? {
            LocPropertyValueRef::VarInt(v) => Some(v),
            // find_property が FindValue::VarInt で濾過しているため実際には到達しない。
            // 値形式の取り違えを型で示すため、網羅性のために残す。
            LocPropertyValueRef::Bytes(_) => None,
        }
    }

    /// 指定 ID の生バイト列を取得する
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の "MUST search both" に
    /// 従い、最上位リストを先に探索し、無ければ IMMUTABLE_PROPERTIES (0x0B) の内側を
    /// delta-key KVP 列として探索する。
    ///
    /// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) は、relay から見える不変の LOC
    /// Properties を IMMUTABLE_PROPERTIES として符号化することを SHOULD とする。
    /// draft-ietf-moq-loc-05 §6.1 (MOQ Properties Registry) の Table 1 は 0x0D
    /// (VIDEO_CONFIG) / 0x0F (AUDIO_CONFIG) の Scope を "Track, Object" と定める。どの
    /// プロパティを immutable にするかは publisher の判断であり、本実装は値の取得時に
    /// transport 側の MUST に従って内側も探索する。
    ///
    /// 内側から取得する場合も外側の `Bytes` を借用したまま返し、値のバイト列を `Vec` に
    /// コピーしない。ID と値形式 (奇数 ID はバイト列) の両方が一致する最初のインスタンスを
    /// 返し、最上位と内側の両方に同じ ID がある場合は最上位を優先する。
    ///
    /// 内側のデコードと検証の失敗、入れ子の IMMUTABLE_PROPERTIES、既知 ID の重複に対する
    /// 扱いは [`LocProperties::find_varint`] と同じである。
    ///
    /// この探索規則と LOC の節番号は draft 由来であり、将来の draft 改版で変わる可能性がある。
    pub fn find_bytes(&self, prop_id: u64) -> Option<&[u8]> {
        match self.find_property(prop_id, FindValue::Bytes)? {
            // find_property が FindValue::Bytes で濾過しているため実際には到達しない。
            // 値形式の取り違えを型で示すため、網羅性のために残す。
            LocPropertyValueRef::VarInt(_) => None,
            LocPropertyValueRef::Bytes(b) => Some(b),
        }
    }

    /// 最上位リストと IMMUTABLE_PROPERTIES の内側から、ID と値形式が一致する最初の
    /// インスタンスを探す
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の "MUST search both" の
    /// 共通探索であり、[`LocProperties::find_varint`] / [`LocProperties::find_bytes`] は
    /// `wanted` を変えて呼ぶだけである。最上位リストを優先し、ID が一致しても値形式が
    /// 一致しないインスタンスは読み飛ばす。最上位リストは多重検証しないが、内側は
    /// [`walk_kv_pairs`] が値域・長さ・重複を検証し、失敗を `None` に写す。詳細は公開
    /// メソッド側の doc を参照。
    fn find_property(&self, prop_id: u64, wanted: FindValue) -> Option<LocPropertyValueRef<'_>> {
        // 最上位リストを先に探索する (最上位優先)
        for p in &self.0 {
            if p.prop_id == prop_id {
                let value = match &p.value {
                    LocPropertyValue::VarInt(v) => LocPropertyValueRef::VarInt(*v),
                    LocPropertyValue::Bytes(b) => LocPropertyValueRef::Bytes(b.as_slice()),
                };
                if value.matches(wanted) {
                    return Some(value);
                }
            }
        }
        // IMMUTABLE_PROPERTIES の内側を探索する。内側の値は外側の Bytes への借用のまま返す。
        let inner = self.immutable_properties()?;
        let props = walk_kv_pairs(inner, false).ok()?;
        props
            .into_iter()
            .find(|p| p.prop_id == prop_id && p.value.matches(wanted))
            .map(|p| p.value)
    }

    /// IMMUTABLE_PROPERTIES (0x0B) の内側の生バイト列を返す
    ///
    /// 中身は入れ子の delta-key KVP 列であり、外側の Properties Length を伴わない
    /// (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) / §8.3
    /// (Key-Value-Pair Structure))。[`LocProperties::find_bytes`] はこのバイト列を借用した
    /// まま内側の値のスライスを返す。
    ///
    /// 最初の IMMUTABLE_PROPERTIES (0x0B) のみを対象とする。0x0B は単一に限られるため、
    /// `decode` は複数インスタンスを `MalformedTrack` で、`encode` は `ProtocolViolation` で
    /// 拒否する。`push` だけで組み立てた入力は検証されないため、その前提が破れた場合の防御
    /// として、先頭の 0x0B がバイト列でない場合も後続の 0x0B を探索せず `None` を返す。
    ///
    /// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) は、relay から見える不変の LOC
    /// Properties をこの形で符号化することを SHOULD とする。
    fn immutable_properties(&self) -> Option<&[u8]> {
        for p in &self.0 {
            if p.prop_id == PROP_IMMUTABLE_PROPERTIES {
                // 0x0B は単一に限られる。先頭の 0x0B がバイト列でない場合は前提が破れた
                // 入力であるため、後続の 0x0B を探索せず打ち切る。
                return match &p.value {
                    LocPropertyValue::Bytes(b) => Some(b.as_slice()),
                    LocPropertyValue::VarInt(_) => None,
                };
            }
        }
        None
    }
}

/// [`LocProperties::find_property`] が探す値の形式
///
/// LOC プロパティは偶数 ID が varint 値、奇数 ID が長さ付きバイト列であるため、ID と値形式の
/// 両方が一致するインスタンスだけを探索結果として扱う。手で組み立てた in-memory 入力では
/// ID が一致しても値形式が異なる場合は読み飛ばす。
#[derive(Debug, Clone, Copy)]
enum FindValue {
    /// 偶数 ID の varint 値
    VarInt,
    /// 奇数 ID の長さ付きバイト列
    Bytes,
}

/// 借用した LOC プロパティ値
///
/// KVP 列の走査結果を保持する内部表現であり、奇数 ID の値は元のバッファへの借用のまま持つ。
/// [`LocProperties::find_bytes`] が IMMUTABLE_PROPERTIES (0x0B) の内側の値を `Vec` へ
/// コピーせず返せるようにするために使う。
#[derive(Debug, Clone, Copy)]
enum LocPropertyValueRef<'a> {
    /// 偶数 ID の値 (varint)
    VarInt(u64),
    /// 奇数 ID の値 (借用したバイト列)
    Bytes(&'a [u8]),
}

impl LocPropertyValueRef<'_> {
    /// 値の形式が `wanted` と一致するかを返す
    fn matches(self, wanted: FindValue) -> bool {
        matches!(
            (self, wanted),
            (Self::VarInt(_), FindValue::VarInt) | (Self::Bytes(_), FindValue::Bytes)
        )
    }
}

/// 借用した LOC プロパティ
///
/// [`walk_kv_pairs`] の走査結果を保持する内部表現であり、奇数 ID の値のバイト列をコピーせず
/// 元のバッファへの借用のまま持つ。
#[derive(Debug, Clone, Copy)]
struct LocPropertyRef<'a> {
    /// プロパティ ID
    prop_id: u64,
    /// プロパティ値
    value: LocPropertyValueRef<'a>,
}

impl LocPropertyRef<'_> {
    /// 所有権を持つ [`LocProperty`] へ変換する
    ///
    /// [`LocProperties::decode`] がワイヤ形式から所有権を持つコレクションを組み立てるために
    /// 使う。バイト列の複製が起きるのはこの変換の 1 回だけである。
    fn to_owned_property(self) -> LocProperty {
        LocProperty {
            prop_id: self.prop_id,
            value: match self.value {
                LocPropertyValueRef::VarInt(v) => LocPropertyValue::VarInt(v),
                LocPropertyValueRef::Bytes(b) => LocPropertyValue::Bytes(b.to_vec()),
            },
        }
    }
}

/// LOC プロパティの Key-Value-Pair 列をバッファ全体から借用して走査する
///
/// カウントプレフィックスなしで `inner` 末尾まで読む。`LocProperties::decode` の本体と、
/// IMMUTABLE_PROPERTIES (0x0B) の内側探索 ([`LocProperties::find_varint`] /
/// [`LocProperties::find_bytes`]) が共有する唯一のパーサーである。奇数 ID の値は `Vec` へ
/// コピーせず `inner` への借用のまま返す。
///
/// 検証は `LocProperties::decode` と同一である。
///
/// - delta-key のオーバーフローを `DeltaEncodingOverflow` で拒否する
///   (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure))
/// - 偶数 ID の値域 (Audio Level が 0-255) を `MalformedKeyValue` で拒否する
/// - 奇数 ID の宣言長が 65535 を超える場合を `FieldLengthExceeded` で拒否する
/// - 奇数 ID の既知プロパティの宣言長 (Video Frame Marking が 1-4 バイト) を
///   `MalformedKeyValue` で拒否する
/// - 単一に限られる既知 ID の重複を `MalformedTrack` で拒否する
///   (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
///   (Malformed Tracks) 条件 9)。未知 ID の重複は §8.4 の "MUST allow multiple
///   instances of the Property" により正当である
///
/// `allow_immutable` が false の場合、IMMUTABLE_PROPERTIES (0x0B) の出現を拒否する。入れ子の
/// IMMUTABLE_PROPERTIES は draft-ietf-moq-transport-23 §10.7 (Immutable Properties) により
/// malformed Track である。`LocProperties::decode` はこの関数を true で呼ぶため、従来どおり
/// 入れ子の 0x0B も構造として保持する (decode の戻り値とエラー分類を変えない)。
fn walk_kv_pairs<'a>(
    inner: &'a [u8],
    allow_immutable: bool,
) -> Result<Vec<LocPropertyRef<'a>>, MessageError> {
    let mut properties = Vec::new();
    let mut inner_pos = 0;
    let mut prev_id: u64 = 0;

    while inner_pos < inner.len() {
        let (prop_id, _delta) = crate::kvp::decode_delta_key(prev_id, inner, &mut inner_pos)?;

        // draft-ietf-moq-transport-23 §10.7 (Immutable Properties): 入れ子の
        // IMMUTABLE_PROPERTIES を検出したら探索を打ち切る。find_varint / find_bytes は
        // このエラーを None に写す。
        if !allow_immutable && prop_id == PROP_IMMUTABLE_PROPERTIES {
            return Err(MessageError::ProtocolViolation(
                "nested IMMUTABLE_PROPERTIES is not allowed (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))",
            ));
        }

        prev_id = prop_id;

        let value = if prop_id % 2 == 0 {
            // 偶数 ID: varint 値
            let (v, n) = varint::decode(&inner[inner_pos..])?;
            inner_pos += n;
            validate_known_property(prop_id, v)?;
            LocPropertyValueRef::VarInt(v)
        } else {
            // 奇数 ID: Length (varint) + bytes
            let (len, n) = varint::decode(&inner[inner_pos..])?;
            inner_pos += n;
            // draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure):
            // "The maximum length of a value is 2^16-1 bytes.  If an endpoint receives a
            // length larger than the maximum, it MUST close the session with
            // FIELD_LENGTH_EXCEEDED."
            if len > 65535 {
                return Err(MessageError::FieldLengthExceeded(
                    "odd type key-value-pair length exceeds 65535",
                ));
            }
            // 既知プロパティ (Video Frame Marking) の宣言長を切り詰め検査より先に検証する
            validate_known_property_len(prop_id, len)?;
            let len = varint::checked_len(len, inner[inner_pos..].len())?;
            let bytes = &inner[inner_pos..inner_pos + len];
            inner_pos += len;
            LocPropertyValueRef::Bytes(bytes)
        };

        properties.push(LocPropertyRef { prop_id, value });
    }

    // 単一に限られる既知 ID の重複を malformed Track として報告する
    // (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
    // (Malformed Tracks) 条件 9)。未知 ID の重複は §8.4 の "MUST allow multiple
    // instances of the Property" により正当である。
    if crate::property_scope::has_duplicate_single_property(properties.iter().map(|p| p.prop_id)) {
        return Err(MessageError::MalformedTrack(DUPLICATE_LOC_PROPERTY));
    }

    Ok(properties)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn audio_config_roundtrip() {
        // Audio Config (奇数 ID = 0x0F) のバイト列がラウンドトリップすること
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(vec![
                0x4F, 0x70, 0x75, 0x73, 0x48, 0x65, 0x61, 0x64, // "OpusHead"
                0x01, // Version
                0x01, // Channel Count
                0x00, 0x00, // Pre-skip
                0x80, 0xBB, 0x00, 0x00, // Input Sample Rate (48000)
                0x00, 0x00, // Output Gain
                0x00, // Channel Mapping Family
            ]),
        });
        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        // OpusHead バイト列がそのまま返ること
        assert_eq!(decoded.audio_config().map(|b| b.len()), Some(19));
        assert_eq!(
            &decoded
                .audio_config()
                .expect("テストフィクスチャの前提条件を満たす")[..8],
            b"OpusHead"
        );
    }

    #[test]
    fn all_known_properties_roundtrip() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0b0001_0001]),
        });
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(9999),
        });
        props.push(LocProperty {
            prop_id: PROP_TIMESCALE,
            value: LocPropertyValue::VarInt(90000),
        });
        props.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(0x50),
        });
        props.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::Bytes(vec![0xAA, 0xBB]),
        });
        props.push(LocProperty {
            prop_id: PROP_AUDIO_CONFIG,
            value: LocPropertyValue::Bytes(vec![0xCC, 0xDD]),
        });
        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.video_frame_marking(), Some(&[0b0001_0001u8][..]));
        assert_eq!(decoded.timestamp(), Some(9999));
        assert_eq!(decoded.timescale(), Some(90000));
        assert_eq!(decoded.audio_level(), Some(0x50));
        assert_eq!(decoded.video_config(), Some(&[0xAAu8, 0xBB][..]));
        assert_eq!(decoded.audio_config(), Some(&[0xCCu8, 0xDD][..]));
    }

    #[test]
    fn even_id_with_bytes_error() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::Bytes(vec![0x01]),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn odd_id_with_varint_error() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_CONFIG,
            value: LocPropertyValue::VarInt(42),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    // ─── 既知プロパティ値域・長さバリデーション ─────────────────────

    #[test]
    fn timestamp_max_accepted() {
        // Timestamp は vi64 1-9 bytes (u64 全域) のため u64::MAX も受理される
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(u64::MAX),
        });
        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, _) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.timestamp(), Some(u64::MAX));
    }

    #[test]
    fn timestamp_max_accepted_on_decode() {
        // u64::MAX (9 バイト varint) の Timestamp を直接ワイヤーフォーマットとして構築してデコード検証
        let mut inner = Vec::new();
        varint::encode(PROP_TIMESTAMP, &mut inner); // delta = prop_id
        varint::encode(u64::MAX, &mut inner); // 9 バイト値
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        let (decoded, _) =
            LocProperties::decode(&buf).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.timestamp(), Some(u64::MAX));
    }

    #[test]
    fn audio_level_max_accepted() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(255),
        });
        let encoded = props
            .encode()
            .expect("正当なテスト入力の encode は成功する");
        let (decoded, _) =
            LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded.audio_level(), Some(255));
    }

    #[test]
    fn audio_level_over_8bit_rejected_on_encode() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_AUDIO_LEVEL,
            value: LocPropertyValue::VarInt(256),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn audio_level_over_8bit_rejected_on_decode() {
        let value = 256u64;
        let mut inner = Vec::new();
        varint::encode(PROP_AUDIO_LEVEL, &mut inner);
        varint::encode(value, &mut inner);
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert!(matches!(
            LocProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    // ─── Video Frame Marking 長さバリデーション ────────────────────

    #[test]
    fn video_frame_marking_roundtrip() {
        // 長さ 1-4 bytes の VFM がラウンドトリップする
        for bytes in [
            vec![0x01u8],
            vec![0x01, 0x02],
            vec![0x01, 0x02, 0x03],
            vec![0x01, 0x02, 0x03, 0x04],
        ] {
            let mut props = LocProperties::new();
            props.push(LocProperty {
                prop_id: PROP_VIDEO_FRAME_MARKING,
                value: LocPropertyValue::Bytes(bytes.clone()),
            });
            let encoded = props
                .encode()
                .expect("正当なテスト入力の encode は成功する");
            let (decoded, _) =
                LocProperties::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
            assert_eq!(decoded.video_frame_marking(), Some(bytes.as_slice()));
        }
    }

    #[test]
    fn video_frame_marking_empty_rejected_on_encode() {
        // 長さ 0 は 1-4 bytes の範囲外
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![]),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn video_frame_marking_empty_rejected_on_decode() {
        // 宣言長 0 の VFM を直接構築してデコード検証
        let mut inner = Vec::new();
        varint::encode(PROP_VIDEO_FRAME_MARKING, &mut inner); // delta = prop_id
        varint::encode(0, &mut inner); // length = 0
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert!(matches!(
            LocProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn video_frame_marking_5_bytes_rejected_on_encode() {
        // 長さ 5 は 1-4 bytes の範囲外
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_VIDEO_FRAME_MARKING,
            value: LocPropertyValue::Bytes(vec![0x01, 0x02, 0x03, 0x04, 0x05]),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn video_frame_marking_5_bytes_rejected_on_decode() {
        // 宣言長 5 の VFM (実バイトは 5) を直接構築してデコード検証
        let mut inner = Vec::new();
        varint::encode(PROP_VIDEO_FRAME_MARKING, &mut inner); // delta = prop_id
        varint::encode(5, &mut inner); // length = 5
        inner.extend_from_slice(&[0x01, 0x02, 0x03, 0x04, 0x05]);
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert!(matches!(
            LocProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn video_frame_marking_declared_len_out_of_range_beats_truncation() {
        // 宣言長 5 (範囲外) で実バイト 2 の入力は UnexpectedEof ではなく
        // MALFORMED_KEY_VALUE になる (宣言長検証が切り詰め検査に優先する)
        let mut inner = Vec::new();
        varint::encode(PROP_VIDEO_FRAME_MARKING, &mut inner); // delta = prop_id
        varint::encode(5, &mut inner); // length = 5 (範囲外)
        inner.extend_from_slice(&[0x01, 0x02]); // 実バイトは 2 のみ
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert!(matches!(
            LocProperties::decode(&buf),
            Err(MessageError::MalformedKeyValue(_))
        ));
    }

    #[test]
    fn video_frame_marking_truncated_within_valid_declared_len() {
        // 宣言長 4 (範囲内) で実バイト 2 の入力は UnexpectedEof になる
        let mut inner = Vec::new();
        varint::encode(PROP_VIDEO_FRAME_MARKING, &mut inner); // delta = prop_id
        varint::encode(4, &mut inner); // length = 4 (範囲内)
        inner.extend_from_slice(&[0x01, 0x02]); // 実バイトは 2 のみ
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert_eq!(
            LocProperties::decode(&buf),
            Err(MessageError::UnexpectedEof)
        );
    }

    // ─── 重複プロパティ ID 検出 ─────────────────────────────────

    #[test]
    fn duplicate_property_id_rejected_on_encode() {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(1000),
        });
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(2000),
        });
        assert!(matches!(
            props.encode(),
            Err(MessageError::ProtocolViolation(_))
        ));
    }

    #[test]
    fn duplicate_property_id_is_malformed_on_decode() {
        // 単一に限られる既知 ID (Timestamp) の重複は malformed Track
        // (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
        // (Malformed Tracks) 条件 9)。delta=0 を含むワイヤーフォーマットを手動構築する
        let mut inner = Vec::new();
        varint::encode(PROP_TIMESTAMP, &mut inner); // delta = 0x10
        varint::encode(1000, &mut inner); // value
        varint::encode(0, &mut inner); // delta = 0 (重複)
        varint::encode(2000, &mut inner); // value
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert!(matches!(
            LocProperties::decode(&buf),
            Err(MessageError::MalformedTrack(_))
        ));
    }

    #[test]
    fn unknown_duplicate_property_id_accepted_on_decode() {
        // 未知 ID の複数インスタンスは §8.4 の "MUST allow multiple instances of the
        // Property" により正当であり、同値・異値のどちらも保持する
        let mut inner = Vec::new();
        varint::encode(0x101, &mut inner); // 未知の奇数 ID (delta = 0x101)
        varint::encode(1, &mut inner); // length = 1
        inner.push(0xAA);
        varint::encode(0, &mut inner); // delta = 0 (同一 ID の 2 個目)
        varint::encode(1, &mut inner); // length = 1
        inner.push(0xBB);
        varint::encode(0, &mut inner); // delta = 0 (同一 ID の 3 個目、同値)
        varint::encode(1, &mut inner); // length = 1
        inner.push(0xBB);
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);

        let (decoded, consumed) =
            LocProperties::decode(&buf).expect("未知 ID の重複は受理されること");
        assert_eq!(consumed, buf.len());
        let values: Vec<&[u8]> = decoded
            .iter()
            .map(|p| match &p.value {
                LocPropertyValue::Bytes(b) => b.as_slice(),
                LocPropertyValue::VarInt(_) => panic!("奇数 ID は Bytes 値を持つこと"),
            })
            .collect();
        assert_eq!(values, vec![&[0xAAu8][..], &[0xBB][..], &[0xBB][..]]);
    }

    #[test]
    fn unknown_duplicate_property_id_roundtrips_on_encode() {
        // 未知 ID の複数インスタンスは encode → decode で順序も含めて往復する
        let mut props = LocProperties::new();
        for value in [0x11u8, 0x22, 0x33] {
            props.push(LocProperty {
                prop_id: 0x101,
                value: LocPropertyValue::Bytes(vec![value]),
            });
        }
        let encoded = props.encode().expect("未知 ID の重複は encode できること");
        let (decoded, consumed) =
            LocProperties::decode(&encoded).expect("未知 ID の重複は decode できること");
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded, props);
    }
}

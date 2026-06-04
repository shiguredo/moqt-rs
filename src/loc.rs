//! Low Overhead Media Container (LOC) プロパティ
//!
//! draft-ietf-moq-loc-04 に基づく実装。
//! 本仕様は draft 由来であり、将来の改訂で変更される可能性がある。
//!
//! LOC Properties は Public と Private に分類される (draft-ietf-moq-loc-04 §2.2):
//!   - LOC Public Properties: MOQ Object Properties に含まれ、リレーから参照可能
//!   - LOC Private Properties: MOQ Object Payload に含まれ、E2E 暗号化で保護可能
//!
//! エンコード形式は draft-ietf-moq-transport-21 §11.1.3 (Object Properties) および draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure) に従う。
//!
//! 本モジュールは Public/Private の区別なくプロパティの encode/decode を提供する。
//! Public/Private の配置はアプリケーション層の責務である。

use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// Audio Level の最大値 (下位 8 bit = 0-255)
///
/// draft-ietf-moq-loc-04 §2.3.3.2: `least significant 8 bits of a vi64`
/// draft の `Value: vi64 (1-2 bytes to encode values 0x00-0xFF)` と整合する。
const AUDIO_LEVEL_MAX: u64 = 255;

/// 既知プロパティの値域を検証する
///
/// draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure):
/// 受信側が既知 Type の Value が定義された serialization と一致しない場合、
/// KEY_VALUE_FORMATTING_ERROR でセッションを閉じなければならない (MUST)。
///
/// 送信側も不正な値を送出しないよう同じ検証を行う。
fn validate_known_property(prop_id: u64, value: u64) -> Result<(), MessageError> {
    match prop_id {
        PROP_AUDIO_LEVEL if value > AUDIO_LEVEL_MAX => Err(MessageError::KeyValueFormattingError(
            "Audio Level value exceeds 8-bit range (draft-ietf-moq-loc-04 §2.3.3.2)",
        )),
        _ => Ok(()),
    }
}

/// 既知プロパティのバイト列長を検証する
///
/// draft-ietf-moq-loc-04 §2.3.2.2: Video Frame Marking の `Length: Varies (1-4 bytes)`。
/// draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure):
/// 受信側が既知 Type の Length が定義された serialization と一致しない場合、
/// KEY_VALUE_FORMATTING_ERROR でセッションを閉じなければならない (MUST)。
///
/// `len` は宣言長 (encode 時は `Bytes` の実長、decode 時は length フィールドの値) であり、
/// 汎用長さ検査 (65535 超過) の直後、decode では切り詰め検査より前に呼ぶ。
fn validate_known_property_len(prop_id: u64, len: u64) -> Result<(), MessageError> {
    match prop_id {
        PROP_VIDEO_FRAME_MARKING if !(1..=4).contains(&len) => {
            Err(MessageError::KeyValueFormattingError(
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

    /// コレクションが空かどうかを返す
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// プロパティの数を返す
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// 全プロパティエントリのイテレータを返す
    pub fn iter(&self) -> impl Iterator<Item = &LocProperty> {
        self.0.iter()
    }

    /// LOC プロパティブロック全体をエンコードする
    ///
    /// prop_id の昇順にソートして delta encoding でエンコードする。
    /// 空のコレクションは Properties Length = 0 としてエンコードする。
    ///
    /// # Errors
    ///
    /// - 偶数 ID に `Bytes` 値: `ProtocolViolation`
    /// - 奇数 ID に `VarInt` 値: `ProtocolViolation`
    /// - 重複するプロパティ ID: `ProtocolViolation`
    /// - バイト列が 65535 バイトを超える: `PayloadTooLong`
    /// - 既知プロパティの値域違反 (Audio Level が 255 超過): `KeyValueFormattingError`
    /// - Video Frame Marking の長さが 1-4 バイトでない: `KeyValueFormattingError`
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        // draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header):
        // "Objects with no properties set Properties Length to 0"
        if self.0.is_empty() {
            let mut buf = Vec::new();
            varint::encode(0, &mut buf);
            return Ok(buf);
        }

        // prop_id の昇順にソート (delta encoding のため差分が常に正)
        let mut sorted = self.0.clone();
        sorted.sort_by_key(|p| p.prop_id);

        // 重複プロパティ ID を検出する
        // delta encoding では同一 Type が複数回出現することを想定していない
        for w in sorted.windows(2) {
            if w[0].prop_id == w[1].prop_id {
                return Err(MessageError::ProtocolViolation("duplicate LOC property ID"));
            }
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
    /// # Errors
    ///
    /// - 重複するプロパティ ID / delta-key のオーバーフロー: `ProtocolViolation`
    /// - 奇数 ID の宣言長が 65535 バイトを超える: `ProtocolViolation`
    /// - Audio Level が 255 を超える: `KeyValueFormattingError`
    /// - Video Frame Marking の宣言長が 1-4 バイトでない: `KeyValueFormattingError`
    ///   (宣言長の検証は切り詰め検査 `UnexpectedEof` より先に行う)
    /// - バッファが不足する: `UnexpectedEof`
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (prop_len, n) = varint::decode(&buf[pos..])?;
        pos += n;

        // draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header):
        // "Objects with no properties set Properties Length to 0"
        if prop_len == 0 {
            return Ok((Self(Vec::new()), pos));
        }

        let prop_len = varint::checked_len(prop_len, buf[pos..].len())?;

        let inner = &buf[pos..pos + prop_len];
        pos += prop_len;

        let mut properties = Vec::new();
        let mut inner_pos = 0;
        let mut prev_id: u64 = 0;

        while inner_pos < inner.len() {
            let (prop_id, delta) = crate::kvp::decode_delta_key(prev_id, inner, &mut inner_pos)?;

            // 重複プロパティ ID を検出する
            // delta == 0 かつ最初のプロパティでない場合は同一 ID の繰り返し
            if delta == 0 && !properties.is_empty() {
                return Err(MessageError::ProtocolViolation("duplicate LOC property ID"));
            }

            prev_id = prop_id;

            let value = if prop_id % 2 == 0 {
                // 偶数 ID: varint 値
                let (v, n) = varint::decode(&inner[inner_pos..])?;
                inner_pos += n;
                validate_known_property(prop_id, v)?;
                LocPropertyValue::VarInt(v)
            } else {
                // 奇数 ID: Length (varint) + bytes
                let (len, n) = varint::decode(&inner[inner_pos..])?;
                inner_pos += n;
                // draft-ietf-moq-transport-21 §8.3 (Key-Value-Pair Structure):
                // "The maximum length of a value is 2^16-1 bytes.
                //  If an endpoint receives a length larger than the maximum,
                //  it MUST close the session with a PROTOCOL_VIOLATION."
                if len > 65535 {
                    return Err(MessageError::ProtocolViolation(
                        "odd type key-value-pair length exceeds 65535",
                    ));
                }
                // 既知プロパティ (Video Frame Marking) の宣言長を切り詰め検査より先に検証する
                validate_known_property_len(prop_id, len)?;
                let len = varint::checked_len(len, inner[inner_pos..].len())?;
                let bytes = inner[inner_pos..inner_pos + len].to_vec();
                inner_pos += len;
                LocPropertyValue::Bytes(bytes)
            };

            properties.push(LocProperty { prop_id, value });
        }

        Ok((Self(properties), pos))
    }

    // ─── 既知プロパティアクセサ ──────────────────────────────────────

    /// Timestamp (ID=0x10): エンコードされたメディアフレームのタイムスタンプ
    ///
    /// draft-ietf-moq-loc-04 §2.3.1.1
    /// Timescale プロパティが存在しない場合は Unix エポック以降のマイクロ秒。
    /// Timescale プロパティが存在する場合は Timescale で定義された単位のメディア時刻。
    pub fn timestamp(&self) -> Option<u64> {
        self.varint_by_id(PROP_TIMESTAMP)
    }

    /// Timescale (ID=0x08): Timestamp の単位を定義する (1 秒あたりのユニット数)
    ///
    /// draft-ietf-moq-loc-04 §2.3.1.2 (Value: vi64 1-9 bytes = u64 全域)
    /// 値域は u64 全域のため、Timestamp と同様に上限検証は不要。
    /// 一般的な値: 1000000 (マイクロ秒), 48000 (48kHz オーディオ), 90000 (90kHz ビデオ)
    /// このプロパティが存在しない場合、Timestamp は Unix エポック以降のマイクロ秒にデフォルトする。
    pub fn timescale(&self) -> Option<u64> {
        self.varint_by_id(PROP_TIMESCALE)
    }

    /// Video Frame Marking (ID=0x09): RFC 9626 に基づくビデオフレームフラグ
    ///
    /// draft-ietf-moq-loc-04 §2.3.2.2 (奇数 ID のバイト列、長さ 1-4 bytes)
    /// フラグの内容は解釈せず、バイト列としてそのまま返す。
    pub fn video_frame_marking(&self) -> Option<&[u8]> {
        self.bytes_by_id(PROP_VIDEO_FRAME_MARKING)
    }

    /// Audio Level (ID=0x0C): RFC 6464 に基づく音声レベルと音声アクティビティ (下位 8 bit)
    ///
    /// draft-ietf-moq-loc-04 §2.3.3.2 (偶数 ID の varint 値、0x00-0xFF)
    pub fn audio_level(&self) -> Option<u64> {
        self.varint_by_id(PROP_AUDIO_LEVEL)
    }

    /// Video Config (ID=0x0D): ビデオコーデック設定 (extradata)
    ///
    /// draft-ietf-moq-loc-04 §2.3.2.1
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn video_config(&self) -> Option<&[u8]> {
        self.bytes_by_id(PROP_VIDEO_CONFIG)
    }

    /// Audio Config (ID=0x0F): 音声コーデックの configuration bytes
    ///
    /// draft-ietf-moq-loc-04 §2.3.3.1
    /// WebCodecs の AudioDecoderConfig description プロパティに対応する。
    /// LOC 層はコーデックの内容を解釈せず、opaque bytes として返す。
    pub fn audio_config(&self) -> Option<&[u8]> {
        self.bytes_by_id(PROP_AUDIO_CONFIG)
    }

    /// 指定 ID の varint 値を検索する内部ヘルパー
    fn varint_by_id(&self, id: u64) -> Option<u64> {
        self.0.iter().find_map(|p| {
            if p.prop_id == id
                && let LocPropertyValue::VarInt(v) = p.value
            {
                return Some(v);
            }
            None
        })
    }

    /// 指定 ID のバイト列値を検索する内部ヘルパー
    fn bytes_by_id(&self, id: u64) -> Option<&[u8]> {
        self.0.iter().find_map(|p| {
            if p.prop_id == id
                && let LocPropertyValue::Bytes(b) = &p.value
            {
                return Some(b.as_slice());
            }
            None
        })
    }
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
            Err(MessageError::KeyValueFormattingError(_))
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
            Err(MessageError::KeyValueFormattingError(_))
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
            Err(MessageError::KeyValueFormattingError(_))
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
            Err(MessageError::KeyValueFormattingError(_))
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
            Err(MessageError::KeyValueFormattingError(_))
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
            Err(MessageError::KeyValueFormattingError(_))
        ));
    }

    #[test]
    fn video_frame_marking_declared_len_out_of_range_beats_truncation() {
        // 宣言長 5 (範囲外) で実バイト 2 の入力は UnexpectedEof ではなく
        // KEY_VALUE_FORMATTING_ERROR になる (宣言長検証が切り詰め検査に優先する)
        let mut inner = Vec::new();
        varint::encode(PROP_VIDEO_FRAME_MARKING, &mut inner); // delta = prop_id
        varint::encode(5, &mut inner); // length = 5 (範囲外)
        inner.extend_from_slice(&[0x01, 0x02]); // 実バイトは 2 のみ
        let mut buf = Vec::new();
        varint::encode(inner.len() as u64, &mut buf);
        buf.extend_from_slice(&inner);
        assert!(matches!(
            LocProperties::decode(&buf),
            Err(MessageError::KeyValueFormattingError(_))
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
    fn duplicate_property_id_rejected_on_decode() {
        // delta=0 を含むワイヤーフォーマットを手動構築する
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
            Err(MessageError::ProtocolViolation(_))
        ));
    }
}

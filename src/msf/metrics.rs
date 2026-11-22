//! MOQMETRICS の Metrics track payload (draft-jennings-moq-metrics-03)
//!
//! draft-jennings-moq-metrics-03 §4 (Metrics Data Model) の Resource / Attribute / Metric を、
//! §5.3 (Catalog Track) の Catalog Header / Metric Description と、§5.4 (Groups and Objects)
//! の Capture Info / Metric Value として encode / decode する。Metric Index は catalog の
//! Object ID と data の Object ID の対応で表現し、Metric Description / Metric Value の
//! payload 自体には含めない (§5.3 / §5.4)。
//!
//! Group ID / Object ID の割り当てと Prior Group ID Gap / Prior Object ID Gap の付与は
//! アプリケーション層の責務であり、Capture Timestamp Offset は Group ID (ミリ秒) に対する
//! ナノ秒オフセットとして扱う (§5.4)。MSF-02 §10.1 / §10.3 の payload / Object ID の記述は
//! MOQMETRICS-03 と食い違うため使わず、MSF-02 §10.4 (Metrics track catalog requirements) の
//! catalog 要件 (packaging "moqmetrics" / role "metrics") に合わせる。この仕様は draft 由来で
//! あり、将来の改版で変更される可能性がある。

use crate::error::MessageError;
use crate::message::common::TrackNamespace;
use crate::msf::MsfPackaging;
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

/// Metrics track の Track Namespace Prefix
///
/// draft-jennings-moq-metrics-03 §5.1 (Track Naming): Track Namespace は 3 つの
/// Track Namespace Field `("metrics.moq.arpa", "v1", <resourceID>)` から成り、この定数は
/// 先頭 2 つの共通 prefix を保持する。ResourceID は 16 桁の小文字 16 進 ASCII で
/// 表現する (§4.1 (ResourceID))。
pub const METRICS_TRACK_NAMESPACE_PREFIX: [&str; 2] = ["metrics.moq.arpa", "v1"];

/// Metrics track の MSF packaging 値
///
/// draft-ietf-moq-msf-02 §10.4 (Metrics track catalog requirements): metrics track は
/// publishTracks で packaging "moqmetrics" を持たなければならない。値は
/// [`MsfPackaging::MoqMetrics`] が表現する。
pub const METRICS_TRACK_PACKAGING: MsfPackaging = MsfPackaging::MoqMetrics;

/// Metrics track の MSF role 値
///
/// draft-ietf-moq-msf-02 §10.4 (Metrics track catalog requirements): metrics track は
/// publishTracks で role "metrics" を持たなければならない。
pub const METRICS_TRACK_ROLE: &str = "metrics";

/// Metrics の catalog track の Track Name
///
/// draft-jennings-moq-metrics-03 §5.1 (Track Naming): resource の namespace に
/// Track Name "catalog" の catalog track が 1 本ある。
pub const METRICS_CATALOG_TRACK_NAME: &[u8] = b"catalog";

/// 本仕様が定義する catalog 形式 version
///
/// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): "Version: The catalog format
/// version. This document defines version 1." Catalog Header の Version はこの値であり、
/// catalog group の Group ID (catalog version 番号) とは別である (§5.3 / §5.4)。
pub const CATALOG_FORMAT_VERSION: u64 = 1;

/// Capture Info の Capture Timestamp Offset の上限
///
/// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): "The value is less than
/// 1000000."
const MAX_CAPTURE_TIMESTAMP_OFFSET_NS: u64 = 999_999;

/// Metrics の granularity level
///
/// draft-jennings-moq-metrics-03 §5.2 (Granularity Levels) の Table 1。data track の
/// Track Name は 1 桁の ASCII 数字で表現する (§5.1 / §5.2)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetricGranularity {
    /// Emergency (Track Name "0") — draft-jennings-moq-metrics-03 §5.2 (Granularity Levels)
    Emergency,
    /// Alert (Track Name "1") — draft-jennings-moq-metrics-03 §5.2 (Granularity Levels)
    Alert,
    /// Critical (Track Name "2") — draft-jennings-moq-metrics-03 §5.2 (Granularity Levels)
    Critical,
    /// Error (Track Name "3") — draft-jennings-moq-metrics-03 §5.2 (Granularity Levels)
    Error,
    /// Warning (Track Name "4") — draft-jennings-moq-metrics-03 §5.2 (Granularity Levels)
    Warning,
    /// Notice (Track Name "5") — draft-jennings-moq-metrics-03 §5.2 (Granularity Levels)
    Notice,
    /// Informational (Track Name "6") — draft-jennings-moq-metrics-03 §5.2 (Granularity Levels)
    Informational,
    /// Debug (Track Name "7") — draft-jennings-moq-metrics-03 §5.2 (Granularity Levels)
    Debug,
}

impl MetricGranularity {
    /// granularity に対応する data track の Track Name ("0"〜"7") を返す
    ///
    /// draft-jennings-moq-metrics-03 §5.1 (Track Naming) / §5.2 (Granularity Levels):
    /// "The Track Name is a single ASCII digit identifying the level."
    pub const fn track_name(self) -> &'static str {
        match self {
            Self::Emergency => "0",
            Self::Alert => "1",
            Self::Critical => "2",
            Self::Error => "3",
            Self::Warning => "4",
            Self::Notice => "5",
            Self::Informational => "6",
            Self::Debug => "7",
        }
    }

    /// Track Name のバイト列から granularity を解釈する
    ///
    /// draft-jennings-moq-metrics-03 §5.1 (Track Naming) / §5.2 (Granularity Levels):
    /// data track の Track Name は granularity level を表す 1 桁の ASCII 数字
    /// ("0"〜"7") である。
    ///
    /// # Errors
    ///
    /// - 1 桁の ASCII 数字 "0"〜"7" 以外: [`MessageError::InvalidMsfPayload`]
    pub fn from_track_name(name: &[u8]) -> Result<Self, MessageError> {
        match name {
            b"0" => Ok(Self::Emergency),
            b"1" => Ok(Self::Alert),
            b"2" => Ok(Self::Critical),
            b"3" => Ok(Self::Error),
            b"4" => Ok(Self::Warning),
            b"5" => Ok(Self::Notice),
            b"6" => Ok(Self::Informational),
            b"7" => Ok(Self::Debug),
            _ => Err(MessageError::InvalidMsfPayload(
                "metrics data track name MUST be a single ASCII digit from 0 to 7".to_string(),
            )),
        }
    }
}

/// Resource の Metrics track namespace を組み立てる
///
/// draft-jennings-moq-metrics-03 §5.1 (Track Naming): Track Namespace は
/// `("metrics.moq.arpa", "v1", <resourceID>)` の 3 field であり、ResourceID は 16 桁の
/// 小文字 16 進 ASCII で表現する (§4.1 (ResourceID))。
pub fn metrics_track_namespace(resource_id: u64) -> TrackNamespace {
    TrackNamespace::new(alloc::vec![
        METRICS_TRACK_NAMESPACE_PREFIX[0].as_bytes().to_vec(),
        METRICS_TRACK_NAMESPACE_PREFIX[1].as_bytes().to_vec(),
        format!("{resource_id:016x}").into_bytes(),
    ])
    .expect("static metrics track namespace fields are always valid")
}

/// Resource の Metrics track namespace を MSF catalog の namespace 文字列にする
///
/// draft-ietf-moq-msf-02 §5.2.2 (Track namespace): §5.1 の 3 field を `/` (U+002F) で
/// 連結する。`metrics.moq.arpa/v1/<16 桁の小文字 hex>` になる。MOQMETRICS-03 §6 の
/// MOQT §8.8 形式は catalog の JSON には使わない。
pub fn metrics_track_namespace_string(resource_id: u64) -> String {
    format!(
        "{}/{}/{:016x}",
        METRICS_TRACK_NAMESPACE_PREFIX[0], METRICS_TRACK_NAMESPACE_PREFIX[1], resource_id
    )
}

/// Metrics の catalog header が持つ resource attribute
///
/// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): Key と Value は UTF-8 文字列であり、
/// resource の Attribute を表す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricAttribute {
    /// Attribute の key (UTF-8)
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): Key は UTF-8 文字列である。
    pub key: String,
    /// Attribute の value (UTF-8)
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): Value は UTF-8 文字列である。
    pub value: String,
}

/// Metrics の Catalog Header
///
/// draft-jennings-moq-metrics-03 §5.3 (Catalog Track) Figure 1。catalog group の
/// Object ID 0 に載る。Version は catalog 形式 version であり、本仕様は
/// [`CATALOG_FORMAT_VERSION`] (= 1) を定義する。catalog group の Group ID は catalog
/// version 番号であり payload には含めない (§5.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogHeader {
    /// catalog 形式 version
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track)。本実装は値の検証を行わない。
    pub version: u64,
    /// resource の Attribute 列 (payload 上の出現順)
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): Number of Attributes の数だけ
    /// Key / Value の対が続く。
    pub attributes: Vec<MetricAttribute>,
}

impl CatalogHeader {
    /// Catalog Header をバイナリ payload にエンコードする
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track) のフィールド順
    /// (Version / Number of Attributes / Attribute...) で出力する。
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        crate::varint::encode(self.version, &mut buf);
        crate::varint::encode(self.attributes.len() as u64, &mut buf);
        for attribute in &self.attributes {
            crate::msf::encode_length_prefixed_bytes(attribute.key.as_bytes(), &mut buf);
            crate::msf::encode_length_prefixed_bytes(attribute.value.as_bytes(), &mut buf);
        }
        buf
    }

    /// バイナリ payload から Catalog Header をデコードする
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): 定義済みフィールドの後ろの
    /// バイトは将来拡張のため無視する。
    ///
    /// # Errors
    ///
    /// - Attribute の key / value が UTF-8 でない: [`MessageError::InvalidMsfPayload`]
    /// - 定義済みフィールドの途中で payload が終わる: [`MessageError::InvalidMsfPayload`]
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        let mut pos = 0;
        let version = crate::msf::decode_payload_varint(buf, &mut pos)?;
        let attribute_count = crate::msf::decode_payload_varint(buf, &mut pos)?;
        // Number of Attributes は入力由来の u64 であり、入力長を超えて読み進められないため
        // Vec の事前確保は行わない (破損入力による過大な確保を防ぐ)。
        let mut attributes = Vec::new();
        for _ in 0..attribute_count {
            let key = decode_utf8_field(buf, &mut pos, "catalog attribute key")?;
            let value = decode_utf8_field(buf, &mut pos, "catalog attribute value")?;
            attributes.push(MetricAttribute { key, value });
        }
        Ok(Self {
            version,
            attributes,
        })
    }
}

/// Metric の value type
///
/// draft-jennings-moq-metrics-03 §4 (Metrics Data Model): "A metric's value can be one of
/// "Gauge" or "Counter"." Metric Type の wire 値は §5.3 (Catalog Track) が定義する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetricType {
    /// Gauge (wire 値 0x0) — draft-jennings-moq-metrics-03 §5.3 (Catalog Track)
    Gauge,
    /// Counter (wire 値 0x1) — draft-jennings-moq-metrics-03 §5.3 (Catalog Track)
    Counter,
}

impl MetricType {
    /// §5.3 (Catalog Track) の Metric Type の wire 値を返す
    pub const fn value(self) -> u64 {
        match self {
            Self::Gauge => 0x0,
            Self::Counter => 0x1,
        }
    }
}

impl TryFrom<u64> for MetricType {
    type Error = MessageError;

    /// Metric Type の wire 値を解釈する
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): "0x0 for Gauge, 0x1 for
    /// Counter". それ以外の値は拒否する。
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0x0 => Ok(Self::Gauge),
            0x1 => Ok(Self::Counter),
            other => Err(MessageError::InvalidMsfPayload(format!(
                "unknown metric type {other}"
            ))),
        }
    }
}

/// Metric Value の Value Representation
///
/// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): Metric Description が data の
/// Object に載る値の encoding を指定する。0x0 は unsigned integer、0x1 は signed
/// integer、0x2 は floating point number である (§5.4 (Groups and Objects))。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueRepresentation {
    /// unsigned integer (wire 値 0x0、vi64) — draft-jennings-moq-metrics-03 §5.3 (Catalog Track)
    Unsigned,
    /// signed integer (wire 値 0x1、64-bit two's complement) — draft-jennings-moq-metrics-03 §5.3 (Catalog Track)
    Signed,
    /// floating point number (wire 値 0x2、IEEE 754 binary64) — draft-jennings-moq-metrics-03 §5.3 (Catalog Track)
    Float,
}

impl ValueRepresentation {
    /// §5.3 (Catalog Track) の Value Representation の wire 値を返す
    pub const fn value(self) -> u64 {
        match self {
            Self::Unsigned => 0x0,
            Self::Signed => 0x1,
            Self::Float => 0x2,
        }
    }
}

impl TryFrom<u64> for ValueRepresentation {
    type Error = MessageError;

    /// Value Representation の wire 値を解釈する
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): "0x0 for an unsigned integer,
    /// 0x1 for a signed integer, 0x2 for a floating point number". それ以外の値は拒否する。
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        match value {
            0x0 => Ok(Self::Unsigned),
            0x1 => Ok(Self::Signed),
            0x2 => Ok(Self::Float),
            other => Err(MessageError::InvalidMsfPayload(format!(
                "unknown value representation {other}"
            ))),
        }
    }
}

/// Metrics の Metric Description
///
/// draft-jennings-moq-metrics-03 §5.3 (Catalog Track) Figure 2。catalog group の
/// Object ID N (N > 0) に載り、[`Self::metric_index`] がその N である。Metric Index は
/// payload には encode せず、catalog group の Object ID と data group の Object ID の
/// 対応で表現する (§5.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetricDescription {
    /// Metric Index (catalog group の Object ID)
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): Object ID N (N > 0) の Metric
    /// Description であり、data group の Object ID N がこの metric の値を運ぶ。payload
    /// には encode しない。
    pub metric_index: u64,
    /// Metric の value type (Gauge / Counter)
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): Metric Type 0x0 は Gauge、
    /// 0x1 は Counter である。
    pub metric_type: MetricType,
    /// data object に載る値の encoding
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track) / §5.4 (Groups and Objects):
    /// 0x0 は unsigned integer、0x1 は signed integer、0x2 は floating point number である。
    pub value_representation: ValueRepresentation,
    /// Metric の名前
    ///
    /// draft-jennings-moq-metrics-03 §4 (Metrics Data Model) / §5.3 (Catalog Track):
    /// ASCII の英字・数字・`_`・`:` から成る。
    pub name: String,
    /// 値の単位
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track): \[OTEL\] の慣習に従う単位。
    /// 未指定の場合は空文字列である。
    pub unit: String,
}

impl MetricDescription {
    /// Metric Description をバイナリ payload にエンコードする
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track) のフィールド順
    /// (Metric Type / Value Representation / Name / Unit) で出力する。Metric Index は
    /// payload に含めない (§5.3)。
    ///
    /// # Errors
    ///
    /// - Metric Index が 0: [`MessageError::InvalidMsfPayload`]
    /// - Name が ASCII の英字・数字・`_`・`:` 以外を含む:
    ///   [`MessageError::InvalidMsfPayload`]
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        validate_metric_index(self.metric_index)?;
        validate_metric_name(&self.name)?;
        let mut buf = Vec::new();
        crate::varint::encode(self.metric_type.value(), &mut buf);
        crate::varint::encode(self.value_representation.value(), &mut buf);
        crate::msf::encode_length_prefixed_bytes(self.name.as_bytes(), &mut buf);
        crate::msf::encode_length_prefixed_bytes(self.unit.as_bytes(), &mut buf);
        Ok(buf)
    }

    /// バイナリ payload から Metric Description をデコードする
    ///
    /// `metric_index` は呼び出し側が catalog group の Object ID として渡す。payload には
    /// Metric Index が含まれないため、この引数が唯一の情報源である (§5.3)。定義済み
    /// フィールドの後ろのバイトは将来拡張のため無視する (§5.3)。
    ///
    /// # Errors
    ///
    /// - Metric Index が 0: [`MessageError::InvalidMsfPayload`]
    /// - Metric Type が 0x0 / 0x1 以外: [`MessageError::InvalidMsfPayload`]
    /// - Value Representation が 0x0 / 0x1 / 0x2 以外: [`MessageError::InvalidMsfPayload`]
    /// - Name が UTF-8 でない、または ASCII の英字・数字・`_`・`:` 以外を含む:
    ///   [`MessageError::InvalidMsfPayload`]
    /// - Unit が UTF-8 でない: [`MessageError::InvalidMsfPayload`]
    /// - 定義済みフィールドの途中で payload が終わる: [`MessageError::InvalidMsfPayload`]
    pub fn decode(buf: &[u8], metric_index: u64) -> Result<Self, MessageError> {
        validate_metric_index(metric_index)?;
        let mut pos = 0;
        let metric_type = MetricType::try_from(crate::msf::decode_payload_varint(buf, &mut pos)?)?;
        let value_representation =
            ValueRepresentation::try_from(crate::msf::decode_payload_varint(buf, &mut pos)?)?;
        let name = decode_metric_name_field(buf, &mut pos)?;
        let unit = decode_utf8_field(buf, &mut pos, "metric unit")?;
        Ok(Self {
            metric_index,
            metric_type,
            value_representation,
            name,
            unit,
        })
    }
}

/// Metrics data group の Capture Info
///
/// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects) Figure 3。data group の
/// Object ID 0 は必須であり、この payload を運ぶ。Group ID は capture time の
/// ミリ秒であり、Capture Timestamp Offset はそこに加算するナノ秒オフセットである。
/// Group ID / Object ID の割り当てはアプリケーション層の責務である。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureInfo {
    /// Group ID (ミリ秒) に加算するナノ秒オフセット
    ///
    /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): 1,000,000 未満でなければ
    /// ならない。
    pub capture_timestamp_offset: u64,
    /// この data group が参照する catalog の Group ID
    ///
    /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): catalog version 番号で
    /// ある。最初の catalog version は Group ID 1 である (§5.3)。
    pub catalog_version: u64,
}

impl CaptureInfo {
    /// Capture Info をバイナリ payload にエンコードする
    ///
    /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects) のフィールド順
    /// (Capture Timestamp Offset / Catalog Version) で出力する。
    ///
    /// # Errors
    ///
    /// - Capture Timestamp Offset が 1,000,000 以上: [`MessageError::InvalidMsfPayload`]
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        validate_capture_timestamp_offset(self.capture_timestamp_offset)?;
        let mut buf = Vec::new();
        crate::varint::encode(self.capture_timestamp_offset, &mut buf);
        crate::varint::encode(self.catalog_version, &mut buf);
        Ok(buf)
    }

    /// バイナリ payload から Capture Info をデコードする
    ///
    /// 定義済みフィールドの後ろのバイトは将来拡張のため無視する。
    ///
    /// # Errors
    ///
    /// - Capture Timestamp Offset が 1,000,000 以上: [`MessageError::InvalidMsfPayload`]
    /// - 定義済みフィールドの途中で payload が終わる: [`MessageError::InvalidMsfPayload`]
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        let mut pos = 0;
        let capture_timestamp_offset = crate::msf::decode_payload_varint(buf, &mut pos)?;
        validate_capture_timestamp_offset(capture_timestamp_offset)?;
        let catalog_version = crate::msf::decode_payload_varint(buf, &mut pos)?;
        Ok(Self {
            capture_timestamp_offset,
            catalog_version,
        })
    }
}

/// Metrics data group の Metric Value
///
/// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): data group の Object ID N
/// (N > 0) の payload 全体が 1 つの Metric Value であり、encoding は参照する catalog の
/// Metric Description の Value Representation に従う。Metric Index (= Object ID) は
/// payload には含めない。
#[derive(Debug, Clone, PartialEq)]
pub enum MetricValue {
    /// unsigned integer (Value Representation 0x0)
    ///
    /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects)
    Unsigned {
        /// Metric Index (data group の Object ID)
        ///
        /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): Object ID N (N > 0) が
        /// Metric Index を表す。
        metric_index: u64,
        /// vi64 で encode する値
        ///
        /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects)。
        value: u64,
    },
    /// signed integer (Value Representation 0x1)
    ///
    /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects)
    Signed {
        /// Metric Index (data group の Object ID)
        ///
        /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): Object ID N (N > 0) が
        /// Metric Index を表す。
        metric_index: u64,
        /// 64-bit two's complement で encode する値
        ///
        /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects)。
        value: i64,
    },
    /// floating point number (Value Representation 0x2)
    ///
    /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects)
    Float {
        /// Metric Index (data group の Object ID)
        ///
        /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): Object ID N (N > 0) が
        /// Metric Index を表す。
        metric_index: u64,
        /// IEEE 754 binary64 で encode する値
        ///
        /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects)。
        value: f64,
    },
}

impl MetricValue {
    /// Metric Index (data group の Object ID) を返す
    ///
    /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects)
    pub const fn metric_index(&self) -> u64 {
        match self {
            Self::Unsigned { metric_index, .. }
            | Self::Signed { metric_index, .. }
            | Self::Float { metric_index, .. } => *metric_index,
        }
    }

    /// 値の Value Representation を返す
    ///
    /// draft-jennings-moq-metrics-03 §5.3 (Catalog Track) / §5.4 (Groups and Objects)
    pub const fn representation(&self) -> ValueRepresentation {
        match self {
            Self::Unsigned { .. } => ValueRepresentation::Unsigned,
            Self::Signed { .. } => ValueRepresentation::Signed,
            Self::Float { .. } => ValueRepresentation::Float,
        }
    }

    /// Metric Value を object payload としてエンコードする
    ///
    /// draft-jennings-moq-metrics-03 §5.4 (Groups and Objects): unsigned integer は vi64、
    /// signed integer は 64-bit two's complement の network byte order、floating point は
    /// IEEE 754 binary64 の network byte order で出力する。Metric Index は payload に
    /// 含めない。
    ///
    /// # Errors
    ///
    /// - Metric Index が 0: [`MessageError::InvalidMsfPayload`]
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        validate_metric_index(self.metric_index())?;
        match self {
            Self::Unsigned { value, .. } => {
                let mut buf = Vec::new();
                crate::varint::encode(*value, &mut buf);
                Ok(buf)
            }
            Self::Signed { value, .. } => Ok(value.to_be_bytes().to_vec()),
            Self::Float { value, .. } => Ok(value.to_be_bytes().to_vec()),
        }
    }

    /// object payload から Metric Value をデコードする
    ///
    /// `metric_index` は data group の Object ID、`representation` は参照する catalog の
    /// Metric Description が持つ Value Representation である (§5.3 / §5.4)。定義済み
    /// フィールドの後ろのバイトは将来拡張のため無視する。
    ///
    /// # Errors
    ///
    /// - Metric Index が 0: [`MessageError::InvalidMsfPayload`]
    /// - unsigned integer の vi64 が途中で終わる: [`MessageError::InvalidMsfPayload`]
    /// - signed integer / floating point の payload が 8 バイト未満:
    ///   [`MessageError::InvalidMsfPayload`]
    pub fn decode(
        buf: &[u8],
        metric_index: u64,
        representation: ValueRepresentation,
    ) -> Result<Self, MessageError> {
        validate_metric_index(metric_index)?;
        match representation {
            ValueRepresentation::Unsigned => {
                let mut pos = 0;
                let value = crate::msf::decode_payload_varint(buf, &mut pos)?;
                Ok(Self::Unsigned {
                    metric_index,
                    value,
                })
            }
            ValueRepresentation::Signed => Ok(Self::Signed {
                metric_index,
                value: decode_i64_payload(buf)?,
            }),
            ValueRepresentation::Float => Ok(Self::Float {
                metric_index,
                value: decode_f64_payload(buf)?,
            }),
        }
    }
}

/// Metric Index が 0 でないことを検証する
///
/// draft-jennings-moq-metrics-03 §5.3 (Catalog Track) / §5.4 (Groups and Objects):
/// Object ID 0 は Catalog Header / Capture Info が使うため、Metric Description と
/// Metric Value の Object ID は N > 0 でなければならない。
fn validate_metric_index(metric_index: u64) -> Result<(), MessageError> {
    if metric_index == 0 {
        return Err(MessageError::InvalidMsfPayload(
            "metric index MUST be greater than 0".to_string(),
        ));
    }
    Ok(())
}

/// Metric 名が §4 の文字種だけから成ることを検証する
///
/// draft-jennings-moq-metrics-03 §4 (Metrics Data Model): "Metric names are represented
/// as strings and may contain ASCII letters, digits, underscores, and colon."
fn validate_metric_name(name: &str) -> Result<(), MessageError> {
    let valid = name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b':');
    if !valid {
        return Err(MessageError::InvalidMsfPayload(
            "metric name MUST contain only ASCII letters, digits, underscores, and colons"
                .to_string(),
        ));
    }
    Ok(())
}

/// Capture Timestamp Offset が §5.4 の上限未満かを検証する
fn validate_capture_timestamp_offset(value: u64) -> Result<(), MessageError> {
    if value > MAX_CAPTURE_TIMESTAMP_OFFSET_NS {
        return Err(MessageError::InvalidMsfPayload(
            "capture timestamp offset MUST be less than 1000000".to_string(),
        ));
    }
    Ok(())
}

/// length-prefixed の UTF-8 文字列フィールドを読み出す
fn decode_utf8_field(
    buf: &[u8],
    pos: &mut usize,
    field: &'static str,
) -> Result<String, MessageError> {
    let bytes = crate::msf::decode_length_prefixed_bytes(buf, pos)?;
    core::str::from_utf8(bytes)
        .map(|text| text.to_string())
        .map_err(|_| MessageError::InvalidMsfPayload(format!("{field} MUST be valid UTF-8")))
}

/// length-prefixed の metric name を読み出して文字種を検証する
fn decode_metric_name_field(buf: &[u8], pos: &mut usize) -> Result<String, MessageError> {
    let bytes = crate::msf::decode_length_prefixed_bytes(buf, pos)?;
    let name = core::str::from_utf8(bytes).map_err(|_| {
        MessageError::InvalidMsfPayload("metric name MUST be valid UTF-8".to_string())
    })?;
    validate_metric_name(name)?;
    Ok(name.to_string())
}

/// 8 バイトの big-endian payload を i64 として読む
fn decode_i64_payload(buf: &[u8]) -> Result<i64, MessageError> {
    let Some(bytes) = buf.get(..8) else {
        return Err(MessageError::InvalidMsfPayload(
            "signed metric value MUST be 8 bytes".to_string(),
        ));
    };
    let mut array = [0u8; 8];
    array.copy_from_slice(bytes);
    Ok(i64::from_be_bytes(array))
}

/// 8 バイトの big-endian payload を f64 として読む
fn decode_f64_payload(buf: &[u8]) -> Result<f64, MessageError> {
    let Some(bytes) = buf.get(..8) else {
        return Err(MessageError::InvalidMsfPayload(
            "floating point metric value MUST be 8 bytes".to_string(),
        ));
    };
    let mut array = [0u8; 8];
    array.copy_from_slice(bytes);
    Ok(f64::from_be_bytes(array))
}

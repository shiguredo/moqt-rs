//! MOQLOG の Log track payload (draft-jennings-moq-log-04)
//!
//! draft-jennings-moq-log-04 §5.1 (Track Naming) の Track Namespace / Track Name と、
//! §5.3 (Log Record) のバイナリ Log Record を扱う。Timestamp Offset は Group ID (秒) に
//! 対するナノ秒オフセットであり (§5.2 (Groups and Objects) / §5.3)、Group ID / Object ID の
//! 割り当てはアプリケーション層の責務である。severity は Track Name で表現され、
//! Log Record には含まれない (§5.3)。
//!
//! MSF-02 §9.1 / §9.3 の payload / Group ID / Object ID の記述は MOQLOG-04 と食い違うため
//! 使わず、MSF-02 §9.4 (Log track catalog requirements) の catalog 要件
//! (packaging "moqlog" / role "log") に合わせる。この仕様は draft 由来であり、将来の
//! 改版で変更される可能性がある。

use crate::error::MessageError;
use crate::message::common::TrackNamespace;
use crate::msf::MsfPackaging;
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};

/// MOQLOG track の Track Namespace Prefix
///
/// draft-jennings-moq-log-04 §5.1 (Track Naming): Track Namespace は 3 つの
/// Track Namespace Field `("logs.moq.arpa", "v1", <resourceID>)` から成り、この定数は
/// 先頭 2 つの共通 prefix を保持する。ResourceID は 16 桁の小文字 16 進 ASCII で
/// 表現する (§4 (Resource ID))。
pub const LOG_TRACK_NAMESPACE_PREFIX: [&str; 2] = ["logs.moq.arpa", "v1"];

/// MOQLOG track の MSF packaging 値
///
/// draft-ietf-moq-msf-02 §9.4 (Log track catalog requirements): log track は
/// publishTracks で packaging "moqlog" を持たなければならない。値は
/// [`MsfPackaging::MoqLog`] が表現する。
pub const LOG_TRACK_PACKAGING: MsfPackaging = MsfPackaging::MoqLog;

/// MOQLOG track の MSF role 値
///
/// draft-ietf-moq-msf-02 §9.4 (Log track catalog requirements): log track は
/// publishTracks で role "log" を持たなければならない。
pub const LOG_TRACK_ROLE: &str = "log";

/// Log Record の Timestamp Offset の上限
///
/// draft-jennings-moq-log-04 §5.3 (Log Record): "The value is less than 1000000000."
const MAX_TIMESTAMP_OFFSET_NS: u64 = 999_999_999;

/// RFC 5424 facility の上限
///
/// draft-jennings-moq-log-04 §5.3 (Log Record): "The facility as defined in \[RFC5424\],
/// from 0 to 23."
const MAX_FACILITY: u64 = 23;

/// MOQLOG の severity level
///
/// draft-jennings-moq-log-04 §5.1 (Track Naming) の Table 1。severity は 1 桁の ASCII
/// 数字の Track Name で表現され、Log Record には含まれない (§5.3 (Log Record))。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogSeverity {
    /// Emergency (Track Name "0") — draft-jennings-moq-log-04 §5.1 (Track Naming)
    Emergency,
    /// Alert (Track Name "1") — draft-jennings-moq-log-04 §5.1 (Track Naming)
    Alert,
    /// Critical (Track Name "2") — draft-jennings-moq-log-04 §5.1 (Track Naming)
    Critical,
    /// Error (Track Name "3") — draft-jennings-moq-log-04 §5.1 (Track Naming)
    Error,
    /// Warning (Track Name "4") — draft-jennings-moq-log-04 §5.1 (Track Naming)
    Warning,
    /// Notice (Track Name "5") — draft-jennings-moq-log-04 §5.1 (Track Naming)
    Notice,
    /// Informational (Track Name "6") — draft-jennings-moq-log-04 §5.1 (Track Naming)
    Informational,
    /// Debug (Track Name "7") — draft-jennings-moq-log-04 §5.1 (Track Naming)
    Debug,
}

impl LogSeverity {
    /// severity に対応する Track Name ("0"〜"7") を返す
    ///
    /// draft-jennings-moq-log-04 §5.1 (Track Naming): "The Track Name is a single ASCII
    /// digit identifying the \[RFC5424\] severity of the log messages it carries."
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

    /// Track Name のバイト列から severity を解釈する
    ///
    /// draft-jennings-moq-log-04 §5.1 (Track Naming): Track Name は severity を表す
    /// 1 桁の ASCII 数字 ("0"〜"7") である。
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
                "log track name MUST be a single ASCII digit from 0 to 7".to_string(),
            )),
        }
    }
}

/// Resource の Log track namespace を組み立てる
///
/// draft-jennings-moq-log-04 §5.1 (Track Naming): Track Namespace は
/// `("logs.moq.arpa", "v1", <resourceID>)` の 3 field であり、ResourceID は 16 桁の
/// 小文字 16 進 ASCII で表現する (§4 (Resource ID))。
pub fn log_track_namespace(resource_id: u64) -> TrackNamespace {
    TrackNamespace::new(alloc::vec![
        LOG_TRACK_NAMESPACE_PREFIX[0].as_bytes().to_vec(),
        LOG_TRACK_NAMESPACE_PREFIX[1].as_bytes().to_vec(),
        format!("{resource_id:016x}").into_bytes(),
    ])
    .expect("static log track namespace fields are always valid")
}

/// Resource の Log track namespace を MSF catalog の namespace 文字列にする
///
/// draft-ietf-moq-msf-02 §5.2.2 (Track namespace): §5.1 の 3 field を `/` (U+002F) で
/// 連結する。`logs.moq.arpa/v1/<16 桁の小文字 hex>` になる。MOQLOG-04 §6 の MOQT §8.8
/// 形式は catalog の JSON には使わない。
pub fn log_track_namespace_string(resource_id: u64) -> String {
    format!(
        "{}/{}/{:016x}",
        LOG_TRACK_NAMESPACE_PREFIX[0], LOG_TRACK_NAMESPACE_PREFIX[1], resource_id
    )
}

/// Log Record の Attribute
///
/// draft-jennings-moq-log-04 §5.3 (Log Record): Key と Value は UTF-8 文字列である。
/// "TraceID" / "SpanID" / "InstrumentationScope" は Attribute の key として扱う。
/// 他の Attribute は \[OTEL\] の Attributes に対応する。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecordAttribute {
    /// Attribute の key (UTF-8)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): Key は UTF-8 文字列である。
    pub key: String,
    /// Attribute の value (UTF-8)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): Value は UTF-8 文字列である。
    pub value: String,
}

/// MOQLOG の Log Record
///
/// draft-jennings-moq-log-04 §5.3 (Log Record) のバイナリ形式。各 log message は
/// 1 つの Object に載り、payload 全体がこの Log Record である (§5.2 (Groups and
/// Objects))。Hostname / App Name / Proc ID / Msg ID の空文字列は RFC 5424 の
/// NILVALUE に対応する (§5.3)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogRecord {
    /// Group ID (秒) に加算するナノ秒オフセット
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): 1,000,000,000 未満でなければ
    /// ならない。Group ID の割り当てはアプリケーション層の責務である (§5.2)。
    pub timestamp_offset: u64,
    /// RFC 5424 facility (0〜23)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): "from 0 to 23".
    pub facility: u64,
    /// RFC 5424 HOSTNAME (ASCII)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): ASCII 文字列であり、空文字列は
    /// NILVALUE に対応する。
    pub hostname: String,
    /// RFC 5424 APP-NAME (ASCII)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): ASCII 文字列であり、空文字列は
    /// NILVALUE に対応する。
    pub app_name: String,
    /// RFC 5424 PROCID (ASCII)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): ASCII 文字列であり、空文字列は
    /// NILVALUE に対応する。
    pub proc_id: String,
    /// RFC 5424 MSGID (ASCII)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): ASCII 文字列であり、空文字列は
    /// NILVALUE に対応する。
    pub msg_id: String,
    /// RFC 5424 MSG / OTEL の Body (UTF-8)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): "The MSG as defined in \[RFC5424\],
    /// as a UTF-8 string. This is the Body in \[OTEL\]."
    pub message: String,
    /// Attribute 列 (payload 上の出現順)
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record): RFC 5424 の structured data を
    /// 運ぶ key-value 対の列である。
    pub attributes: Vec<LogRecordAttribute>,
}

impl LogRecord {
    /// Log Record をバイナリ payload にエンコードする
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record) のフィールド順
    /// (Timestamp Offset / Facility / Hostname / App Name / Proc ID / Msg ID / Message /
    /// Number of Attributes / Attribute...) で出力する。vi64 は
    /// draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers) の形式である。
    ///
    /// # Errors
    ///
    /// - Timestamp Offset が 1,000,000,000 以上: [`MessageError::InvalidMsfPayload`]
    /// - Facility が 23 を超える: [`MessageError::InvalidMsfPayload`]
    /// - Hostname / App Name / Proc ID / Msg ID が ASCII でない:
    ///   [`MessageError::InvalidMsfPayload`]
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        validate_timestamp_offset(self.timestamp_offset)?;
        validate_facility(self.facility)?;
        validate_ascii_field("hostname", &self.hostname)?;
        validate_ascii_field("app name", &self.app_name)?;
        validate_ascii_field("proc id", &self.proc_id)?;
        validate_ascii_field("msg id", &self.msg_id)?;

        let mut buf = Vec::new();
        crate::varint::encode(self.timestamp_offset, &mut buf);
        crate::varint::encode(self.facility, &mut buf);
        crate::msf::encode_length_prefixed_bytes(self.hostname.as_bytes(), &mut buf);
        crate::msf::encode_length_prefixed_bytes(self.app_name.as_bytes(), &mut buf);
        crate::msf::encode_length_prefixed_bytes(self.proc_id.as_bytes(), &mut buf);
        crate::msf::encode_length_prefixed_bytes(self.msg_id.as_bytes(), &mut buf);
        crate::msf::encode_length_prefixed_bytes(self.message.as_bytes(), &mut buf);
        crate::varint::encode(self.attributes.len() as u64, &mut buf);
        for attribute in &self.attributes {
            crate::msf::encode_length_prefixed_bytes(attribute.key.as_bytes(), &mut buf);
            crate::msf::encode_length_prefixed_bytes(attribute.value.as_bytes(), &mut buf);
        }
        Ok(buf)
    }

    /// バイナリ payload から Log Record をデコードする
    ///
    /// draft-jennings-moq-log-04 §5.3 (Log Record) のフィールド順で読み、Number of
    /// Attributes の数だけ Attribute を読む。定義済みフィールドの後ろのバイトは
    /// 将来拡張のため無視する (§5.3)。severity は Track Name で運ばれ、Log Record には
    /// 含まれない (§5.1 / §5.3)。
    ///
    /// # Errors
    ///
    /// - Timestamp Offset が 1,000,000,000 以上: [`MessageError::InvalidMsfPayload`]
    /// - Facility が 23 を超える: [`MessageError::InvalidMsfPayload`]
    /// - Hostname / App Name / Proc ID / Msg ID が ASCII でない:
    ///   [`MessageError::InvalidMsfPayload`]
    /// - Message / Attribute の key / value が UTF-8 でない:
    ///   [`MessageError::InvalidMsfPayload`]
    /// - 定義済みフィールドの途中で payload が終わる: [`MessageError::InvalidMsfPayload`]
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        let mut pos = 0;
        let timestamp_offset = crate::msf::decode_payload_varint(buf, &mut pos)?;
        validate_timestamp_offset(timestamp_offset)?;
        let facility = crate::msf::decode_payload_varint(buf, &mut pos)?;
        validate_facility(facility)?;
        let hostname = decode_ascii_field(buf, &mut pos, "hostname")?;
        let app_name = decode_ascii_field(buf, &mut pos, "app name")?;
        let proc_id = decode_ascii_field(buf, &mut pos, "proc id")?;
        let msg_id = decode_ascii_field(buf, &mut pos, "msg id")?;
        let message = decode_utf8_field(buf, &mut pos, "message")?;
        let attribute_count = crate::msf::decode_payload_varint(buf, &mut pos)?;
        // Number of Attributes は入力由来の u64 であり、極端に大きい値でも入力長までしか
        // 読み進められないため、Vec の事前確保は行わない (破損入力による過大な確保を防ぐ)。
        let mut attributes = Vec::new();
        for _ in 0..attribute_count {
            let key = decode_utf8_field(buf, &mut pos, "attribute key")?;
            let value = decode_utf8_field(buf, &mut pos, "attribute value")?;
            attributes.push(LogRecordAttribute { key, value });
        }
        Ok(Self {
            timestamp_offset,
            facility,
            hostname,
            app_name,
            proc_id,
            msg_id,
            message,
            attributes,
        })
    }
}

/// Timestamp Offset が §5.3 の上限未満かを検証する
fn validate_timestamp_offset(value: u64) -> Result<(), MessageError> {
    if value > MAX_TIMESTAMP_OFFSET_NS {
        return Err(MessageError::InvalidMsfPayload(
            "log record timestamp offset MUST be less than 1000000000".to_string(),
        ));
    }
    Ok(())
}

/// Facility が §5.3 の値域かを検証する
fn validate_facility(value: u64) -> Result<(), MessageError> {
    if value > MAX_FACILITY {
        return Err(MessageError::InvalidMsfPayload(
            "log record facility MUST be between 0 and 23".to_string(),
        ));
    }
    Ok(())
}

/// ASCII 文字列でなければならないフィールドを検証する
fn validate_ascii_field(field: &'static str, value: &str) -> Result<(), MessageError> {
    if !value.is_ascii() {
        return Err(MessageError::InvalidMsfPayload(format!(
            "{field} MUST be an ASCII string"
        )));
    }
    Ok(())
}

/// length-prefixed の ASCII 文字列フィールドを読み出す
fn decode_ascii_field(
    buf: &[u8],
    pos: &mut usize,
    field: &'static str,
) -> Result<String, MessageError> {
    let bytes = crate::msf::decode_length_prefixed_bytes(buf, pos)?;
    if !bytes.is_ascii() {
        return Err(MessageError::InvalidMsfPayload(format!(
            "{field} MUST be an ASCII string"
        )));
    }
    core::str::from_utf8(bytes)
        .map(|text| text.to_string())
        .map_err(|_| MessageError::InvalidMsfPayload(format!("{field} MUST be an ASCII string")))
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

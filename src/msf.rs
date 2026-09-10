//! MOQT Streaming Format (MSF) 実装
//!
//! draft-ietf-moq-msf-01 に基づく。
//! カタログ、メディアタイムライン、イベントタイムラインの JSON エンコード/デコードを提供する。
//!
//! # カタログの種類
//! - Full catalog: `{"version":"draft-01","tracks":[...]}` 形式
//! - Delta update: `{"deltaUpdate":[...]}` 形式
//!
//! # 注意
//! カタログトラックのトラック名は "catalog" でなければならない (draft-ietf-moq-msf-01 §5 (Catalog))。

use crate::error::MessageError;
use alloc::{
    format,
    string::{String, ToString},
    vec::Vec,
};
use nojson::{DisplayJson, JsonFormatter};

/// MSF URI / fragment のパース (draft-ietf-moq-msf-01 §11.1 (URL construction and interpretation))
pub mod uri;

/// 対応 MSF バージョン (draft-ietf-moq-msf-01 §5.1.1 (MSF version))
///
/// draft-ietf-moq-msf-01 §5.1.1 (MSF version): version は JSON Type: String である。
/// IETF Internet-Draft 利用時は "draft-XX" 形式を使う。
/// この仕様は将来変更される可能性がある。
pub const MSF_VERSION: &str = "draft-01";

/// カタログトラックの Track Name (draft-ietf-moq-msf-01 §5 (Catalog))
///
/// "The catalog track MUST have a case-sensitive Track Name of 'catalog'."
/// この仕様は将来変更される可能性がある。
pub const MSF_CATALOG_TRACK_NAME: &[u8] = b"catalog";

/// nojson のパースエラーを MessageError に変換する
impl From<nojson::JsonParseError> for MessageError {
    fn from(e: nojson::JsonParseError) -> Self {
        MessageError::InvalidCatalog(format!("JSON parse error at position {}", e.position()))
    }
}

/// JSON 整数値を u64 としてパースする
fn parse_u64(v: nojson::RawJsonValue<'_, '_>) -> Result<u64, nojson::JsonParseError> {
    v.as_integer_str()?.parse::<u64>().map_err(|e| v.invalid(e))
}

/// JSON 数値を有限の f64 としてパースする
///
/// 非有限値 (infinity / NaN) は JSON の数値として不正なため拒否する (RFC 8259 §6 (Numbers))。
fn parse_finite_f64(v: nojson::RawJsonValue<'_, '_>) -> Result<f64, nojson::JsonParseError> {
    let f = v
        .as_number_str()?
        .parse::<f64>()
        .map_err(|e| v.invalid(e))?;
    if !f.is_finite() {
        return Err(v.invalid("f64 field must be finite"));
    }
    Ok(f)
}

/// lang フィールドの軽量構文バリデーション
///
/// draft-ietf-moq-msf-01 §5.2.32 (Language): lang は BCP 47 言語タグでなければならない (MUST)
/// この仕様は将来変更される可能性がある。
/// 完全な BCP 47 準拠は行わず、RFC 5646 §2.1 (Syntax) に沿った簡略ルールで
/// 明らかに不正な値のみを拒否する:
/// - primary language subtag: 2〜3 文字 / 4 文字 / 5〜8 文字の ASCII 英字 (language)
/// - privateuse tag: `x` に 1 個以上の `-` 区切りサブタグが続くもの
/// - 後続サブタグ（任意）: `-` 区切り、各サブタグは 1 文字以上の ASCII 英数字
fn validate_lang_tag(lang: &str) -> Result<(), MessageError> {
    let mut parts = lang.split('-');
    let primary = parts.next().expect("split never returns an empty iterator");
    // RFC 5646 §2.1 (Syntax): privateuse = "x" 1*("-" (1*8alphanum))
    if primary == "x" {
        let mut has_subtag = false;
        for subtag in parts {
            has_subtag = true;
            if subtag.is_empty() || !subtag.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(MessageError::InvalidCatalog(format!(
                    "invalid language tag '{lang}': each privateuse subtag must be non-empty ASCII alphanumeric"
                )));
            }
        }
        if !has_subtag {
            return Err(MessageError::InvalidCatalog(format!(
                "invalid language tag '{lang}': privateuse tag must have at least one subtag"
            )));
        }
        return Ok(());
    }
    // RFC 5646 §2.1 (Syntax): language = 2*3ALPHA / 4ALPHA / 5*8ALPHA
    if primary.len() < 2 || primary.len() > 8 || !primary.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(MessageError::InvalidCatalog(format!(
            "invalid language tag '{lang}': primary language subtag must be 2-8 ASCII letters"
        )));
    }
    for subtag in parts {
        if subtag.is_empty() || !subtag.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(MessageError::InvalidCatalog(format!(
                "invalid language tag '{lang}': each subtag must be non-empty ASCII alphanumeric"
            )));
        }
    }
    Ok(())
}

/// 生 JSON バイト列をそのまま出力するラッパー
struct RawJsonBytes<'a>(&'a [u8]);

impl DisplayJson for RawJsonBytes<'_> {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        let s = core::str::from_utf8(self.0).map_err(|_| core::fmt::Error)?;
        write!(f.inner_mut(), "{s}")
    }
}

// ─── パブリック型定義 ─────────────────────────────────────────────────────────

/// MSF パッケージングタイプ (draft-ietf-moq-msf-01 §5.2.4 (Packaging))
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MsfPackaging {
    /// LOC パッケージング ("loc")
    Loc,
    /// メディアタイムライン ("mediatimeline")
    MediaTimeline,
    /// イベントタイムライン ("eventtimeline")
    EventTimeline,
    /// moqlog ("moqlog")
    MoqLog,
    /// moqmetrics ("moqmetrics")
    MoqMetrics,
}

impl MsfPackaging {
    fn as_str(&self) -> &str {
        match self {
            Self::Loc => "loc",
            Self::MediaTimeline => "mediatimeline",
            Self::EventTimeline => "eventtimeline",
            Self::MoqLog => "moqlog",
            Self::MoqMetrics => "moqmetrics",
        }
    }

    /// draft-ietf-moq-msf-01 §5.2.4 (Packaging): 許容値は loc / mediatimeline / eventtimeline / moqlog / moqmetrics
    /// この仕様は将来変更される可能性がある。
    fn from_packaging_str(s: &str) -> Result<Self, MessageError> {
        match s {
            "loc" => Ok(Self::Loc),
            "mediatimeline" => Ok(Self::MediaTimeline),
            "eventtimeline" => Ok(Self::EventTimeline),
            "moqlog" => Ok(Self::MoqLog),
            "moqmetrics" => Ok(Self::MoqMetrics),
            other => Err(MessageError::InvalidCatalog(format!(
                "unknown packaging value '{other}'"
            ))),
        }
    }
}

/// 初期化データの種別 (draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List))
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MsfInitDataKind {
    /// インライン Base64 ("inline")
    Inline,
}

impl DisplayJson for MsfInitDataKind {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        match self {
            Self::Inline => f.value("inline"),
        }
    }
}

impl MsfInitDataKind {
    fn from_init_data_kind_str(s: &str) -> Result<Self, MessageError> {
        match s {
            "inline" => Ok(Self::Inline),
            other => Err(MessageError::InvalidCatalog(format!(
                "unknown initData type '{other}'"
            ))),
        }
    }
}

/// 初期化データエントリ (draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List))
#[derive(Debug, Clone, PartialEq)]
pub struct MsfInitData {
    /// エントリを識別する ID (draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List))
    pub id: String,
    /// 初期化データの種別 (JSON key "type")
    pub kind: MsfInitDataKind,
    /// 初期化データ (draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List))
    pub data: String,
}

impl DisplayJson for MsfInitData {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            f.member("id", self.id.as_str())?;
            // JSON key は "type" だが Rust 予約語のためフィールド名は kind としている
            f.member("type", self.kind)?;
            f.member("data", self.data.as_str())
        })
    }
}

/// ターゲットバッファ情報 (draft-ietf-moq-msf-01 §5.2.9 (Buffers))
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MsfBuffers {
    /// ターゲットバッファ ms
    pub target: Option<u64>,
    /// 最小バッファ ms
    pub min: Option<u64>,
    /// 最大バッファ ms
    pub max: Option<u64>,
}

impl DisplayJson for MsfBuffers {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            if let Some(v) = self.target {
                f.member("target", v)?;
            }
            if let Some(v) = self.min {
                f.member("min", v)?;
            }
            if let Some(v) = self.max {
                f.member("max", v)?;
            }
            Ok(())
        })
    }
}

/// メディアタイムラインテンプレート (draft-ietf-moq-msf-01 §5.2.15 (Template) / §7.4.1 (Template Format))
///
/// 6 要素は必須であり指定順に現れる。計算式は §7.4.1 に従う。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MsfTemplate {
    /// 先頭エントリのメディア PTS ms
    pub start_media_time: u64,
    /// メディア PTS の間隔 ms
    pub delta_media_time: u64,
    /// 先頭エントリの Group ID
    pub start_group_id: u64,
    /// 先頭エントリの Object ID
    pub start_object_id: u64,
    /// Group ID の間隔
    pub delta_group_id: u64,
    /// Object ID の間隔
    pub delta_object_id: u64,
    /// 先頭エントリのウォールクロック ms (不明な場合は 0)
    pub start_wallclock: u64,
    /// ウォールクロックの間隔 ms (不明な場合は 0)
    pub delta_wallclock: u64,
}

impl DisplayJson for MsfTemplate {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.array(|f| {
            f.element(self.start_media_time)?;
            f.element(self.delta_media_time)?;
            f.element(nojson::array(|f| {
                f.element(self.start_group_id)?;
                f.element(self.start_object_id)
            }))?;
            f.element(nojson::array(|f| {
                f.element(self.delta_group_id)?;
                f.element(self.delta_object_id)
            }))?;
            f.element(self.start_wallclock)?;
            f.element(self.delta_wallclock)
        })
    }
}

impl MsfTemplate {
    /// n 番目 (0 始まり) のエントリを求める
    ///
    /// draft-ietf-moq-msf-01 §7.4.1 の計算式に従う。4 系列のいずれかが overflow した場合は
    /// 全体を `None` とする (仕様式に overflow 定義がないための防御的扱い)。
    pub fn resolve_entry(&self, n: u64) -> Option<MsfMediaTimelineEntry> {
        let media_time = self
            .delta_media_time
            .checked_mul(n)?
            .checked_add(self.start_media_time)?;
        let group_id = self
            .delta_group_id
            .checked_mul(n)?
            .checked_add(self.start_group_id)?;
        let object_id = self
            .delta_object_id
            .checked_mul(n)?
            .checked_add(self.start_object_id)?;
        let wallclock_ms = self
            .delta_wallclock
            .checked_mul(n)?
            .checked_add(self.start_wallclock)?;
        Some(MsfMediaTimelineEntry {
            pts_ms: media_time,
            group_id,
            object_id,
            wallclock_ms,
        })
    }
}

/// 認可情報エントリ (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info))
///
/// 値は scheme 固有であり、任意の JSON 値を生保持する (string / object を含む)。
/// §5.2.42 本文は configuration object と述べるが、§5.2.43 と §5.6.15 の例は
/// 文字列値 (`"%token%"` 等) を使うため、両方を受理する。
/// scheme 名の検証 (RDNN 等) は行わない。
///
/// `value_raw` は単独の JSON 値でなければならず、`MsfCatalogDocument::encode` で検証する。
/// 不正値を手構築すると `encode` が `InvalidCatalog` で失敗するため、単一 JSON 値にすること。
/// 空 object は `Some` の空 `Vec` として保持し、重複 scheme は順序どおりに保持する。
#[derive(Debug, Clone, PartialEq)]
pub struct MsfAuthInfo {
    /// 認可 scheme 名
    pub scheme: String,
    /// scheme 固有値の生 JSON
    pub value_raw: Vec<u8>,
}

/// authInfo object の encode 用ラッパー
struct AuthInfoEntries<'a>(&'a [MsfAuthInfo]);

impl DisplayJson for AuthInfoEntries<'_> {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            for info in self.0 {
                f.member(info.scheme.as_str(), RawJsonBytes(&info.value_raw))?;
            }
            Ok(())
        })
    }
}

/// accessibility 記述子 (draft-ietf-moq-msf-01 §5.2.44 (Accessibility))
#[derive(Debug, Clone, PartialEq)]
pub struct MsfAccessibility {
    /// 記述子 scheme
    pub scheme: String,
    /// 記述子値
    pub value: String,
}

impl DisplayJson for MsfAccessibility {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            f.member("scheme", self.scheme.as_str())?;
            f.member("value", self.value.as_str())
        })
    }
}

/// MSF トラックオブジェクト (draft-ietf-moq-msf-01 §5.2 (Track Object Fields))
#[derive(Debug, Clone, PartialEq)]
pub struct MsfTrack {
    /// トラック名 (draft-ietf-moq-msf-01 §5.2.3 (Track name)) - 必須
    pub name: String,
    /// トラックネームスペース (draft-ietf-moq-msf-01 §5.2.2 (Track namespace))
    pub namespace: Option<String>,
    /// パッケージングタイプ (draft-ietf-moq-msf-01 §5.2.4 (Packaging)) - 必須
    pub packaging: MsfPackaging,
    /// イベントタイムラインタイプ (draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)) - packaging=eventtimeline の場合必須
    pub event_type: Option<String>,
    /// トラックロール (draft-ietf-moq-msf-01 §5.2.6 (Track role))
    pub role: Option<String>,
    /// ライブフラグ (draft-ietf-moq-msf-01 §5.2.7 (Is Live)) - 必須
    pub is_live: bool,
    /// ターゲットレイテンシ ms (draft-ietf-moq-msf-01 §5.2.8 (Target latency))
    ///
    /// isLive=false の場合は無視され encode されない。
    pub target_latency: Option<u64>,
    /// ターゲットバッファ情報 (draft-ietf-moq-msf-01 §5.2.9 (Buffers))
    ///
    /// isLive=false の場合は無視され encode されない。
    pub buffers: Option<MsfBuffers>,
    /// トラックラベル (draft-ietf-moq-msf-01 §5.2.10 (Track label))
    pub label: Option<String>,
    /// レンダーグループ (draft-ietf-moq-msf-01 §5.2.11 (Render group))
    pub render_group: Option<u64>,
    /// オルタネートグループ (draft-ietf-moq-msf-01 §5.2.12 (Alternate group))
    pub alt_group: Option<u64>,
    /// 初期化データ参照 (draft-ietf-moq-msf-01 §5.2.13 (Initialization reference))
    pub init_ref: Option<String>,
    /// 依存トラック名配列 (draft-ietf-moq-msf-01 §5.2.14 (Dependencies))
    pub depends: Vec<String>,
    /// メディアタイムラインテンプレート (draft-ietf-moq-msf-01 §5.2.15 (Template))
    pub template: Option<MsfTemplate>,
    /// テンポラル ID (draft-ietf-moq-msf-01 §5.2.16 (Temporal ID))
    pub temporal_id: Option<u64>,
    /// スペーシャル ID (draft-ietf-moq-msf-01 §5.2.17 (Spatial ID))
    pub spatial_id: Option<u64>,
    /// コーデック (draft-ietf-moq-msf-01 §5.2.18 (Codec))
    pub codec: Option<String>,
    /// MIME タイプ (draft-ietf-moq-msf-01 §5.2.19 (Mimetype))
    pub mime_type: Option<String>,
    /// フレームレート fps (draft-ietf-moq-msf-01 §5.2.20 (Framerate))
    pub framerate: Option<f64>,
    /// タイムスケール (draft-ietf-moq-msf-01 §5.2.21 (Timescale))
    pub timescale: Option<u64>,
    /// 最大ビットレート bps (draft-ietf-moq-msf-01 §5.2.22 (Maximum Bitrate))
    pub bitrate: Option<u64>,
    /// 平均ビットレート bps (draft-ietf-moq-msf-01 §5.2.23 (Average Bitrate))
    pub avg_bitrate: Option<u64>,
    /// 最大 GOP 長 ms (draft-ietf-moq-msf-01 §5.2.24 (Maximum GOP Duration))
    pub max_gop_duration: Option<u64>,
    /// 最大 Group 長 ms (draft-ietf-moq-msf-01 §5.2.25 (Maximum Group Duration))
    pub max_group_duration: Option<u64>,
    /// エンコード幅 px (draft-ietf-moq-msf-01 §5.2.26 (Width))
    pub width: Option<u64>,
    /// エンコード高さ px (draft-ietf-moq-msf-01 §5.2.27 (Height))
    pub height: Option<u64>,
    /// オーディオサンプルレート Hz (draft-ietf-moq-msf-01 §5.2.28 (Audio sample rate))
    pub samplerate: Option<u64>,
    /// チャンネル設定 (draft-ietf-moq-msf-01 §5.2.29 (Channel configuration))
    pub channel_config: Option<String>,
    /// 表示幅 px (draft-ietf-moq-msf-01 §5.2.30 (Display width))
    pub display_width: Option<u64>,
    /// 表示高さ px (draft-ietf-moq-msf-01 §5.2.31 (Display height))
    pub display_height: Option<u64>,
    /// 言語タグ (draft-ietf-moq-msf-01 §5.2.32 (Language))
    pub lang: Option<String>,
    /// 親トラック名 (draft-ietf-moq-msf-01 §5.2.33 (Parent name)) - cloneTracks 内のみ
    pub parent_name: Option<String>,
    /// トラック長 ms (draft-ietf-moq-msf-01 §5.2.35 (Track duration))
    pub track_duration: Option<u64>,
    /// 接続先 URI (draft-ietf-moq-msf-01 §5.2.36 (Connection URI))
    pub connection_uri: Option<String>,
    /// 認証トークン (draft-ietf-moq-msf-01 §5.2.37 (Token))
    pub token: Option<String>,
    /// 暗号化方式 (draft-ietf-moq-msf-01 §5.2.38 (Encryption Scheme))
    pub encryption_scheme: Option<String>,
    /// 暗号スイート (draft-ietf-moq-msf-01 §5.2.39 (Cipher Suite))
    pub cipher_suite: Option<String>,
    /// 鍵識別子 (draft-ietf-moq-msf-01 §5.2.40 (Key ID))
    pub key_id: Option<String>,
    /// track 基本鍵 (draft-ietf-moq-msf-01 §5.2.41 (Track Base Key))
    pub track_base_key: Option<String>,
    /// 認可情報 (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info))
    pub auth_info: Option<Vec<MsfAuthInfo>>,
    /// accessibility 記述子列 (draft-ietf-moq-msf-01 §5.2.44 (Accessibility))
    pub accessibility: Vec<MsfAccessibility>,
}

impl MsfTrack {
    /// 必須フィールドのみで MsfTrack を作成する
    pub fn new(name: String, packaging: MsfPackaging, is_live: bool) -> Self {
        Self {
            name,
            packaging,
            namespace: None,
            event_type: None,
            role: None,
            is_live,
            target_latency: None,
            buffers: None,
            label: None,
            render_group: None,
            alt_group: None,
            init_ref: None,
            depends: Vec::new(),
            template: None,
            temporal_id: None,
            spatial_id: None,
            codec: None,
            mime_type: None,
            framerate: None,
            timescale: None,
            bitrate: None,
            avg_bitrate: None,
            max_gop_duration: None,
            max_group_duration: None,
            width: None,
            height: None,
            samplerate: None,
            channel_config: None,
            display_width: None,
            display_height: None,
            lang: None,
            parent_name: None,
            track_duration: None,
            connection_uri: None,
            token: None,
            encryption_scheme: None,
            cipher_suite: None,
            key_id: None,
            track_base_key: None,
            auth_info: None,
            accessibility: Vec::new(),
        }
    }
}

impl DisplayJson for MsfTrack {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            f.member("name", self.name.as_str())?;
            f.member("packaging", self.packaging.as_str())?;
            if let Some(ref v) = self.namespace {
                f.member("namespace", v.as_str())?;
            }
            if let Some(ref v) = self.event_type {
                f.member("eventType", v.as_str())?;
            }
            if let Some(ref v) = self.role {
                f.member("role", v.as_str())?;
            }
            f.member("isLive", self.is_live)?;
            // decode 側が isLive=false で targetLatency / buffers を None に正規化するため、
            // encode も省略して対称にする。draft-ietf-moq-msf-01 §5.2.8 / §5.2.9 は
            // 受信側の無視規則であり、エンコーダの出力禁止ではない。
            if self.is_live {
                if let Some(v) = self.target_latency {
                    f.member("targetLatency", v)?;
                }
                if let Some(ref v) = self.buffers {
                    f.member("buffers", v)?;
                }
            }
            if let Some(ref v) = self.label {
                f.member("label", v.as_str())?;
            }
            if let Some(v) = self.render_group {
                f.member("renderGroup", v)?;
            }
            if let Some(v) = self.alt_group {
                f.member("altGroup", v)?;
            }
            if let Some(ref v) = self.init_ref {
                f.member("initRef", v.as_str())?;
            }
            if !self.depends.is_empty() {
                f.member(
                    "depends",
                    nojson::array(|f| f.elements(self.depends.iter().map(|s| s.as_str()))),
                )?;
            }
            if let Some(ref v) = self.template {
                f.member("template", v)?;
            }
            if let Some(v) = self.temporal_id {
                f.member("temporalId", v)?;
            }
            if let Some(v) = self.spatial_id {
                f.member("spatialId", v)?;
            }
            if let Some(ref v) = self.codec {
                f.member("codec", v.as_str())?;
            }
            if let Some(ref v) = self.mime_type {
                f.member("mimeType", v.as_str())?;
            }
            if let Some(v) = self.framerate {
                f.member("framerate", v)?;
            }
            if let Some(v) = self.timescale {
                f.member("timescale", v)?;
            }
            if let Some(v) = self.bitrate {
                f.member("bitrate", v)?;
            }
            if let Some(v) = self.avg_bitrate {
                f.member("avgBitrate", v)?;
            }
            if let Some(v) = self.max_gop_duration {
                f.member("maxGopDuration", v)?;
            }
            if let Some(v) = self.max_group_duration {
                f.member("maxGroupDuration", v)?;
            }
            if let Some(v) = self.width {
                f.member("width", v)?;
            }
            if let Some(v) = self.height {
                f.member("height", v)?;
            }
            if let Some(v) = self.samplerate {
                f.member("samplerate", v)?;
            }
            if let Some(ref v) = self.channel_config {
                f.member("channelConfig", v.as_str())?;
            }
            if let Some(v) = self.display_width {
                f.member("displayWidth", v)?;
            }
            if let Some(v) = self.display_height {
                f.member("displayHeight", v)?;
            }
            if let Some(ref v) = self.lang {
                f.member("lang", v.as_str())?;
            }
            if let Some(ref v) = self.parent_name {
                f.member("parentName", v.as_str())?;
            }
            if let Some(v) = self.track_duration {
                f.member("trackDuration", v)?;
            }
            if let Some(ref v) = self.connection_uri {
                f.member("connectionUri", v.as_str())?;
            }
            if let Some(ref v) = self.token {
                f.member("token", v.as_str())?;
            }
            if let Some(ref v) = self.encryption_scheme {
                f.member("encryptionScheme", v.as_str())?;
            }
            if let Some(ref v) = self.cipher_suite {
                f.member("cipherSuite", v.as_str())?;
            }
            if let Some(ref v) = self.key_id {
                f.member("keyId", v.as_str())?;
            }
            if let Some(ref v) = self.track_base_key {
                f.member("trackBaseKey", v.as_str())?;
            }
            if let Some(ref infos) = self.auth_info {
                f.member("authInfo", AuthInfoEntries(infos))?;
            }
            if !self.accessibility.is_empty() {
                f.member(
                    "accessibility",
                    nojson::array(|f| f.elements(self.accessibility.iter())),
                )?;
            }
            Ok(())
        })
    }
}

/// cloneTracks 内のトラック定義 (draft-ietf-moq-msf-01 §5.1.6 (Delta update))
///
/// draft-ietf-moq-msf-01 §5.1.6 (Delta update): cloned track は親の全属性を継承し、
/// 再定義された属性のみ上書きする。
/// Track Name は新規でなければならない。
/// この仕様は将来変更される可能性がある。
#[derive(Debug, Clone, PartialEq)]
pub struct MsfCloneTrack {
    /// トラック名 (draft-ietf-moq-msf-01 §5.2.3 (Track name)) - 必須、新規でなければならない
    pub name: String,
    /// 親トラック名 (draft-ietf-moq-msf-01 §5.2.33 (Parent name)) - 必須
    pub parent_name: String,
    /// 親トラックネームスペース (draft-ietf-moq-msf-01 §5.2.34 (Parent namespace))
    ///
    /// `None` のときはカタログのネームスペースが親のネームスペースと仮定される。
    /// この仕様は将来変更される可能性がある。
    pub parent_namespace: Option<String>,
    /// パッケージングタイプ (draft-ietf-moq-msf-01 §5.2.4 (Packaging)) - 省略時は親から継承
    pub packaging: Option<MsfPackaging>,
    /// トラックネームスペース (draft-ietf-moq-msf-01 §5.2.2 (Track namespace))
    pub namespace: Option<String>,
    /// イベントタイムラインタイプ (draft-ietf-moq-msf-01 §5.2.5 (Event timeline type))
    pub event_type: Option<String>,
    /// トラックロール (draft-ietf-moq-msf-01 §5.2.6 (Track role))
    pub role: Option<String>,
    /// ライブフラグ (draft-ietf-moq-msf-01 §5.2.7 (Is Live)) - 省略時は親から継承
    pub is_live: Option<bool>,
    /// ターゲットレイテンシ ms (draft-ietf-moq-msf-01 §5.2.8 (Target latency))
    ///
    /// isLive が Some(false) の場合は無視され encode されない。None (親から継承) と Some(true) では出力する。
    pub target_latency: Option<u64>,
    /// ターゲットバッファ情報 (draft-ietf-moq-msf-01 §5.2.9 (Buffers))
    ///
    /// isLive が Some(false) の場合は無視され encode されない。None (親から継承) と Some(true) では出力する。
    pub buffers: Option<MsfBuffers>,
    /// トラックラベル (draft-ietf-moq-msf-01 §5.2.10 (Track label))
    pub label: Option<String>,
    /// レンダーグループ (draft-ietf-moq-msf-01 §5.2.11 (Render group))
    pub render_group: Option<u64>,
    /// オルタネートグループ (draft-ietf-moq-msf-01 §5.2.12 (Alternate group))
    pub alt_group: Option<u64>,
    /// 初期化データ参照 (draft-ietf-moq-msf-01 §5.2.13 (Initialization reference))
    pub init_ref: Option<String>,
    /// 依存トラック名配列 (draft-ietf-moq-msf-01 §5.2.14 (Dependencies)) - None は親から継承
    pub depends: Option<Vec<String>>,
    /// メディアタイムラインテンプレート (draft-ietf-moq-msf-01 §5.2.15 (Template)) - None は親から継承
    pub template: Option<MsfTemplate>,
    /// テンポラル ID (draft-ietf-moq-msf-01 §5.2.16 (Temporal ID))
    pub temporal_id: Option<u64>,
    /// スペーシャル ID (draft-ietf-moq-msf-01 §5.2.17 (Spatial ID))
    pub spatial_id: Option<u64>,
    /// コーデック (draft-ietf-moq-msf-01 §5.2.18 (Codec))
    pub codec: Option<String>,
    /// MIME タイプ (draft-ietf-moq-msf-01 §5.2.19 (Mimetype))
    pub mime_type: Option<String>,
    /// フレームレート fps (draft-ietf-moq-msf-01 §5.2.20 (Framerate))
    pub framerate: Option<f64>,
    /// タイムスケール (draft-ietf-moq-msf-01 §5.2.21 (Timescale))
    pub timescale: Option<u64>,
    /// 最大ビットレート bps (draft-ietf-moq-msf-01 §5.2.22 (Maximum Bitrate))
    pub bitrate: Option<u64>,
    /// 平均ビットレート bps (draft-ietf-moq-msf-01 §5.2.23 (Average Bitrate))
    pub avg_bitrate: Option<u64>,
    /// 最大 GOP 長 ms (draft-ietf-moq-msf-01 §5.2.24 (Maximum GOP Duration))
    pub max_gop_duration: Option<u64>,
    /// 最大 Group 長 ms (draft-ietf-moq-msf-01 §5.2.25 (Maximum Group Duration))
    pub max_group_duration: Option<u64>,
    /// エンコード幅 px (draft-ietf-moq-msf-01 §5.2.26 (Width))
    pub width: Option<u64>,
    /// エンコード高さ px (draft-ietf-moq-msf-01 §5.2.27 (Height))
    pub height: Option<u64>,
    /// オーディオサンプルレート Hz (draft-ietf-moq-msf-01 §5.2.28 (Audio sample rate))
    pub samplerate: Option<u64>,
    /// チャンネル設定 (draft-ietf-moq-msf-01 §5.2.29 (Channel configuration))
    pub channel_config: Option<String>,
    /// 表示幅 px (draft-ietf-moq-msf-01 §5.2.30 (Display width))
    pub display_width: Option<u64>,
    /// 表示高さ px (draft-ietf-moq-msf-01 §5.2.31 (Display height))
    pub display_height: Option<u64>,
    /// 言語タグ (draft-ietf-moq-msf-01 §5.2.32 (Language))
    pub lang: Option<String>,
    /// トラック長 ms (draft-ietf-moq-msf-01 §5.2.35 (Track duration))
    pub track_duration: Option<u64>,
    /// 接続先 URI (draft-ietf-moq-msf-01 §5.2.36 (Connection URI))
    pub connection_uri: Option<String>,
    /// 認証トークン (draft-ietf-moq-msf-01 §5.2.37 (Token))
    pub token: Option<String>,
    /// 暗号化方式 (draft-ietf-moq-msf-01 §5.2.38 (Encryption Scheme))
    pub encryption_scheme: Option<String>,
    /// 暗号スイート (draft-ietf-moq-msf-01 §5.2.39 (Cipher Suite))
    pub cipher_suite: Option<String>,
    /// 鍵識別子 (draft-ietf-moq-msf-01 §5.2.40 (Key ID))
    pub key_id: Option<String>,
    /// track 基本鍵 (draft-ietf-moq-msf-01 §5.2.41 (Track Base Key))
    pub track_base_key: Option<String>,
    /// 認可情報 (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info)) - None は親から継承
    pub auth_info: Option<Vec<MsfAuthInfo>>,
    /// accessibility 記述子列 (draft-ietf-moq-msf-01 §5.2.44 (Accessibility)) - None は親から継承
    pub accessibility: Option<Vec<MsfAccessibility>>,
}

impl MsfCloneTrack {
    /// name / parentName のみを指定して clone トラック定義を作成する
    ///
    /// 他の属性はすべて未指定 (`None`) とし、親トラックから継承する
    /// (draft-ietf-moq-msf-01 §5.1.6 (Delta update))。
    pub fn new(name: String, parent_name: String) -> Self {
        Self {
            name,
            parent_name,
            parent_namespace: None,
            packaging: None,
            namespace: None,
            event_type: None,
            role: None,
            is_live: None,
            target_latency: None,
            buffers: None,
            label: None,
            render_group: None,
            alt_group: None,
            init_ref: None,
            depends: None,
            template: None,
            temporal_id: None,
            spatial_id: None,
            codec: None,
            mime_type: None,
            framerate: None,
            timescale: None,
            bitrate: None,
            avg_bitrate: None,
            max_gop_duration: None,
            max_group_duration: None,
            width: None,
            height: None,
            samplerate: None,
            channel_config: None,
            display_width: None,
            display_height: None,
            lang: None,
            track_duration: None,
            connection_uri: None,
            token: None,
            encryption_scheme: None,
            cipher_suite: None,
            key_id: None,
            track_base_key: None,
            auth_info: None,
            accessibility: None,
        }
    }
}

impl DisplayJson for MsfCloneTrack {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            f.member("name", self.name.as_str())?;
            f.member("parentName", self.parent_name.as_str())?;
            if let Some(ref v) = self.parent_namespace {
                f.member("parentNamespace", v.as_str())?;
            }
            if let Some(ref v) = self.packaging {
                f.member("packaging", v.as_str())?;
            }
            if let Some(ref v) = self.namespace {
                f.member("namespace", v.as_str())?;
            }
            if let Some(ref v) = self.event_type {
                f.member("eventType", v.as_str())?;
            }
            if let Some(ref v) = self.role {
                f.member("role", v.as_str())?;
            }
            if let Some(v) = self.is_live {
                f.member("isLive", v)?;
            }
            // clone は is_live == None (親から継承) を保持するため、Some(false) のときだけ
            // targetLatency / buffers を省略する。draft-ietf-moq-msf-01 §5.2.8 / §5.2.9 は
            // 受信側の無視規則であり、decode の正規化に encode を合わせる。
            if matches!(self.is_live, None | Some(true)) {
                if let Some(v) = self.target_latency {
                    f.member("targetLatency", v)?;
                }
                if let Some(ref v) = self.buffers {
                    f.member("buffers", v)?;
                }
            }
            if let Some(ref v) = self.label {
                f.member("label", v.as_str())?;
            }
            if let Some(v) = self.render_group {
                f.member("renderGroup", v)?;
            }
            if let Some(v) = self.alt_group {
                f.member("altGroup", v)?;
            }
            if let Some(ref v) = self.init_ref {
                f.member("initRef", v.as_str())?;
            }
            if let Some(ref deps) = self.depends {
                // clone は None (継承) と Some([]) (空上書き) を区別するため空でも出力する
                f.member(
                    "depends",
                    nojson::array(|f| f.elements(deps.iter().map(|s| s.as_str()))),
                )?;
            }
            if let Some(ref v) = self.template {
                f.member("template", v)?;
            }
            if let Some(v) = self.temporal_id {
                f.member("temporalId", v)?;
            }
            if let Some(v) = self.spatial_id {
                f.member("spatialId", v)?;
            }
            if let Some(ref v) = self.codec {
                f.member("codec", v.as_str())?;
            }
            if let Some(ref v) = self.mime_type {
                f.member("mimeType", v.as_str())?;
            }
            if let Some(v) = self.framerate {
                f.member("framerate", v)?;
            }
            if let Some(v) = self.timescale {
                f.member("timescale", v)?;
            }
            if let Some(v) = self.bitrate {
                f.member("bitrate", v)?;
            }
            if let Some(v) = self.avg_bitrate {
                f.member("avgBitrate", v)?;
            }
            if let Some(v) = self.max_gop_duration {
                f.member("maxGopDuration", v)?;
            }
            if let Some(v) = self.max_group_duration {
                f.member("maxGroupDuration", v)?;
            }
            if let Some(v) = self.width {
                f.member("width", v)?;
            }
            if let Some(v) = self.height {
                f.member("height", v)?;
            }
            if let Some(v) = self.samplerate {
                f.member("samplerate", v)?;
            }
            if let Some(ref v) = self.channel_config {
                f.member("channelConfig", v.as_str())?;
            }
            if let Some(v) = self.display_width {
                f.member("displayWidth", v)?;
            }
            if let Some(v) = self.display_height {
                f.member("displayHeight", v)?;
            }
            if let Some(ref v) = self.lang {
                f.member("lang", v.as_str())?;
            }
            if let Some(v) = self.track_duration {
                f.member("trackDuration", v)?;
            }
            if let Some(ref v) = self.connection_uri {
                f.member("connectionUri", v.as_str())?;
            }
            if let Some(ref v) = self.token {
                f.member("token", v.as_str())?;
            }
            if let Some(ref v) = self.encryption_scheme {
                f.member("encryptionScheme", v.as_str())?;
            }
            if let Some(ref v) = self.cipher_suite {
                f.member("cipherSuite", v.as_str())?;
            }
            if let Some(ref v) = self.key_id {
                f.member("keyId", v.as_str())?;
            }
            if let Some(ref v) = self.track_base_key {
                f.member("trackBaseKey", v.as_str())?;
            }
            if let Some(ref infos) = self.auth_info {
                f.member("authInfo", AuthInfoEntries(infos))?;
            }
            if let Some(ref descs) = self.accessibility {
                // clone は None (継承) と Some([]) (空上書き) を区別するため空でも出力する
                f.member("accessibility", nojson::array(|f| f.elements(descs.iter())))?;
            }
            Ok(())
        })
    }
}

impl MsfCloneTrack {
    /// 親トラックの属性を継承し、clone で再定義された属性で上書きしたトラックを生成する
    ///
    /// draft-ietf-moq-msf-01 §5.1.6 (Delta update): "The cloned track inherits all
    /// attributes from the parent except the Track Name which MUST be new.
    /// Attributes redefined in the track object override inherited values."
    /// この仕様は将来変更される可能性がある。
    ///
    /// # Errors
    ///
    /// 継承後のトラックが §5.2.5 (Event timeline type) / §5.2.7 (Is Live) /
    /// §5.2.35 (Track duration) / §5.2.39 (Cipher suite) / §4.3.3 (Recommended encryption scheme) /
    /// §7.2 (Media Timeline Catalog requirements) / §8.2 (Event Timeline Catalog requirements)
    /// の制約に違反する場合、`InvalidCatalog` を返す。
    pub fn into_track(self, parent: &MsfTrack) -> Result<MsfTrack, MessageError> {
        // draft-ietf-moq-msf-01 §5.1.6 (Delta update): 再定義が無い属性は親から継承する
        let packaging = self.packaging.unwrap_or(parent.packaging);
        let event_type = self.event_type.or_else(|| parent.event_type.clone());
        let role = self.role.or_else(|| parent.role.clone());
        let is_live = self.is_live.unwrap_or(parent.is_live);
        let label = self.label.or_else(|| parent.label.clone());
        let render_group = self.render_group.or(parent.render_group);
        let alt_group = self.alt_group.or(parent.alt_group);
        let init_ref = self.init_ref.or_else(|| parent.init_ref.clone());
        let depends = self.depends.unwrap_or_else(|| parent.depends.clone());
        let template = self.template.or(parent.template);
        let temporal_id = self.temporal_id.or(parent.temporal_id);
        let spatial_id = self.spatial_id.or(parent.spatial_id);
        let codec = self.codec.or_else(|| parent.codec.clone());
        let mime_type = self.mime_type.or_else(|| parent.mime_type.clone());
        let framerate = self.framerate.or(parent.framerate);
        let timescale = self.timescale.or(parent.timescale);
        let bitrate = self.bitrate.or(parent.bitrate);
        let avg_bitrate = self.avg_bitrate.or(parent.avg_bitrate);
        let max_gop_duration = self.max_gop_duration.or(parent.max_gop_duration);
        let max_group_duration = self.max_group_duration.or(parent.max_group_duration);
        let width = self.width.or(parent.width);
        let height = self.height.or(parent.height);
        let samplerate = self.samplerate.or(parent.samplerate);
        let channel_config = self
            .channel_config
            .or_else(|| parent.channel_config.clone());
        let display_width = self.display_width.or(parent.display_width);
        let display_height = self.display_height.or(parent.display_height);
        let lang = self.lang.or_else(|| parent.lang.clone());
        let track_duration = self.track_duration.or(parent.track_duration);
        let connection_uri = self
            .connection_uri
            .or_else(|| parent.connection_uri.clone());
        let token = self.token.or_else(|| parent.token.clone());
        let encryption_scheme = self
            .encryption_scheme
            .or_else(|| parent.encryption_scheme.clone());
        let cipher_suite = self.cipher_suite.or_else(|| parent.cipher_suite.clone());
        let key_id = self.key_id.or_else(|| parent.key_id.clone());
        let track_base_key = self
            .track_base_key
            .or_else(|| parent.track_base_key.clone());
        let auth_info = self.auth_info.or_else(|| parent.auth_info.clone());
        let accessibility = self
            .accessibility
            .unwrap_or_else(|| parent.accessibility.clone());
        // clone 自身の namespace 指定が無ければ親の namespace を継承する
        let namespace = self.namespace.or_else(|| parent.namespace.clone());

        // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers):
        // isLive=false なら targetLatency / buffers は無視されるため継承分も含めて落とす
        let (target_latency, buffers) = if is_live {
            (
                self.target_latency.or(parent.target_latency),
                self.buffers.or(parent.buffers),
            )
        } else {
            (None, None)
        };

        // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers):
        // 継承解決後に targetLatency と buffers が共存してはならない。
        // clone 側と親側で別々に指定された場合もここで検出する。
        if target_latency.is_some() && buffers.is_some() {
            return Err(MessageError::InvalidCatalog(
                "targetLatency and buffers MUST NOT be present together".to_string(),
            ));
        }

        // draft-ietf-moq-msf-01 §5.2.35 (Track duration): isLive=true なら trackDuration は禁止
        if is_live && track_duration.is_some() {
            return Err(MessageError::InvalidCatalog(
                "trackDuration MUST NOT be included when isLive is true".to_string(),
            ));
        }

        // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type): eventtimeline なら eventType 必須、それ以外では禁止
        let is_event_timeline = matches!(packaging, MsfPackaging::EventTimeline);
        if is_event_timeline && event_type.is_none() {
            return Err(MessageError::InvalidCatalog(
                "eventType is required when packaging is 'eventtimeline'".to_string(),
            ));
        }
        if !is_event_timeline && event_type.is_some() {
            return Err(MessageError::InvalidCatalog(
                "eventType MUST NOT be used when packaging is not 'eventtimeline'".to_string(),
            ));
        }

        // draft-ietf-moq-msf-01 §7.2 (Media Timeline Catalog requirements) / §8.2 (Event Timeline Catalog requirements):
        // timeline は depends 必須、mimeType=application/json 必須
        let is_media_timeline = matches!(packaging, MsfPackaging::MediaTimeline);
        if is_media_timeline {
            if depends.is_empty() {
                return Err(MessageError::InvalidCatalog(
                    "mediatimeline track MUST have a 'depends' attribute".to_string(),
                ));
            }
            if mime_type.as_deref() != Some("application/json") {
                return Err(MessageError::InvalidCatalog(
                    "mediatimeline track mimeType MUST be 'application/json'".to_string(),
                ));
            }
        }
        if is_event_timeline {
            if depends.is_empty() {
                return Err(MessageError::InvalidCatalog(
                    "eventtimeline track MUST have a 'depends' attribute".to_string(),
                ));
            }
            if mime_type.as_deref() != Some("application/json") {
                return Err(MessageError::InvalidCatalog(
                    "eventtimeline track mimeType MUST be 'application/json'".to_string(),
                ));
            }
        }

        // draft-ietf-moq-msf-01 §5.2.39 (Cipher suite) / §4.3.3 (Recommended encryption scheme):
        // 暗号化 signaling の必須組
        if encryption_scheme.is_some() && cipher_suite.is_none() {
            return Err(MessageError::InvalidCatalog(
                "cipherSuite MUST be present when encryptionScheme is specified".to_string(),
            ));
        }
        if encryption_scheme.as_deref() == Some("moq-secure-objects")
            && (key_id.is_none() || track_base_key.is_none())
        {
            return Err(MessageError::InvalidCatalog(
                "keyId and trackBaseKey MUST be present with moq-secure-objects".to_string(),
            ));
        }

        let track = MsfTrack {
            name: self.name,
            namespace,
            packaging,
            event_type,
            role,
            is_live,
            target_latency,
            buffers,
            label,
            render_group,
            alt_group,
            init_ref,
            depends,
            template,
            temporal_id,
            spatial_id,
            codec,
            mime_type,
            framerate,
            timescale,
            bitrate,
            avg_bitrate,
            max_gop_duration,
            max_group_duration,
            width,
            height,
            samplerate,
            channel_config,
            display_width,
            display_height,
            lang,
            // 解決済みトラックは clone 元の parentName を保持しない
            parent_name: None,
            track_duration,
            connection_uri,
            token,
            encryption_scheme,
            cipher_suite,
            key_id,
            track_base_key,
            auth_info,
            accessibility,
        };
        validate_media_track_fields(&track)?;
        Ok(track)
    }
}

/// removeTracks 内のトラック参照 (draft-ietf-moq-msf-01 §5.1.6 (Delta update))
///
/// name と namespace のみ保持する。
#[derive(Debug, Clone, PartialEq)]
pub struct MsfRemoveTrack {
    /// トラック名 (必須)
    pub name: String,
    /// トラックネームスペース
    pub namespace: Option<String>,
}

impl MsfRemoveTrack {
    /// name のみを指定して削除対象トラック参照を作成する
    ///
    /// namespace は未指定 (`None`) とし、カタログの namespace を継承したものとして扱う
    /// (draft-ietf-moq-msf-01 §5.1.6 (Delta update))。
    pub fn new(name: String) -> Self {
        Self {
            name,
            namespace: None,
        }
    }
}

impl DisplayJson for MsfRemoveTrack {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            f.member("name", self.name.as_str())?;
            if let Some(ref ns) = self.namespace {
                f.member("namespace", ns.as_str())?;
            }
            Ok(())
        })
    }
}

/// Full カタログ (draft-ietf-moq-msf-01 §5.1 (Root Catalog Fields))
#[derive(Debug, Clone, PartialEq)]
pub struct MsfCatalog {
    /// MSF バージョン (draft-ietf-moq-msf-01 §5.1.1 (MSF version))
    pub version: String,
    /// カタログ生成時刻 ms (draft-ietf-moq-msf-01 §5.1.2 (Generated at))
    pub generated_at: Option<u64>,
    /// ブロードキャスト完了フラグ (draft-ietf-moq-msf-01 §5.1.3 (Is Complete))
    pub is_complete: bool,
    /// トラック一覧 (draft-ietf-moq-msf-01 §5.1.4 (Tracks))
    pub tracks: Vec<MsfTrack>,
    /// publish track 一覧 (draft-ietf-moq-msf-01 §5.1.5 (Publish tracks))
    ///
    /// 未知フィールドは無視される (§5 前文の MUST ignore)。
    pub publish_tracks: Vec<MsfTrack>,
    /// 初期化データ一覧 (draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List))
    pub init_data_list: Vec<MsfInitData>,
}

impl MsfCatalog {
    /// 空のカタログを作成する
    ///
    /// version は対応バージョン ([`MSF_VERSION`]) とし、tracks / publishTracks /
    /// initDataList は空とする。
    pub fn new() -> Self {
        Self {
            version: MSF_VERSION.to_string(),
            generated_at: None,
            is_complete: false,
            tracks: Vec::new(),
            publish_tracks: Vec::new(),
            init_data_list: Vec::new(),
        }
    }
}

impl Default for MsfCatalog {
    fn default() -> Self {
        Self::new()
    }
}

impl DisplayJson for MsfCatalog {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            f.member("version", self.version.as_str())?;
            if let Some(ga) = self.generated_at {
                f.member("generatedAt", ga)?;
            }
            if self.is_complete {
                f.member("isComplete", true)?;
            }
            f.member("tracks", nojson::array(|f| f.elements(self.tracks.iter())))?;
            // draft-ietf-moq-msf-01 §5.1.5 (Publish tracks): 空配列のときは出力しない
            // (§5.1.5 は Optional とするのみで、省略はエンコーダの約束である)
            if !self.publish_tracks.is_empty() {
                f.member(
                    "publishTracks",
                    nojson::array(|f| f.elements(self.publish_tracks.iter())),
                )?;
            }
            // エンコーダの慣例として tracks → publishTracks → initDataList の順で出力する
            // (initDataList が tracks より後という §5.1.7 の MUST を満たし、間は Table 1 の記載順に従う。
            // JSON としての意味は変わらない)
            // draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List): initDataList は tracks 配列より後に出現しなければならない
            // 空配列のときは出力しない
            if !self.init_data_list.is_empty() {
                f.member(
                    "initDataList",
                    nojson::array(|f| f.elements(self.init_data_list.iter())),
                )?;
            }
            Ok(())
        })
    }
}

impl MsfCatalog {
    /// デルタ更新をこのカタログへ適用する
    ///
    /// draft-ietf-moq-msf-01 §5.1.6 (Delta update) / §5.3 (Delta updates): 操作は配列順に
    /// 適用される。clone は親トラックの属性を継承し、再定義された属性で上書きする。
    /// この仕様は将来変更される可能性がある。
    ///
    /// `catalog_namespace` はカタログトラック自身のネームスペースであり、トラックが
    /// namespace を省略した場合の継承先 (§5.2.2 (Track namespace)) として使う。
    ///
    /// # Errors
    ///
    /// - `isComplete` が true のカタログへの add / clone: `InvalidCatalog`
    ///   (draft-ietf-moq-msf-01 §5.1.3 (Is Complete))
    /// - add / clone のトラック名が既存と重複する: `InvalidCatalog`
    /// - remove / clone の対象トラックが見つからない: `InvalidCatalog`
    /// - clone の継承結果がトラックの制約に違反する: `InvalidCatalog`
    /// - 適用後のカタログがグループ内 targetLatency / buffers の一致
    ///   (§5.2.8 (Target latency) / §5.2.9 (Buffers)) に違反する: `InvalidCatalog`
    /// - 適用後の initRef が initDataList の id を指さない: `InvalidCatalog`
    ///   (§5.2.13 (Initialization reference))
    pub fn apply_delta(
        &mut self,
        delta: &MsfDeltaUpdate,
        catalog_namespace: Option<&str>,
    ) -> Result<(), MessageError> {
        for op in &delta.operations {
            match op {
                MsfDeltaOperation::Add { tracks } => {
                    // draft-ietf-moq-msf-01 §5.1.3 (Is Complete): "no new tracks will be
                    // added to the catalog" のため、isComplete=true への add は拒否する。
                    // remove は同節が禁じていないため許可する。
                    if self.is_complete {
                        return Err(MessageError::InvalidCatalog(
                            "delta add: catalog is complete; no new tracks may be added"
                                .to_string(),
                        ));
                    }
                    for track in tracks {
                        let ns = track.namespace.as_deref().or(catalog_namespace);
                        if self
                            .find_track_index(ns, &track.name, catalog_namespace)
                            .is_some()
                        {
                            return Err(MessageError::InvalidCatalog(format!(
                                "delta add: track '{}' already exists",
                                track.name
                            )));
                        }
                        self.tracks.push(track.clone());
                    }
                }
                MsfDeltaOperation::Remove { tracks } => {
                    for r in tracks {
                        let ns = r.namespace.as_deref().or(catalog_namespace);
                        let idx = self
                            .find_track_index(ns, &r.name, catalog_namespace)
                            .ok_or_else(|| {
                                MessageError::InvalidCatalog(format!(
                                    "delta remove: track '{}' not found",
                                    r.name
                                ))
                            })?;
                        self.tracks.remove(idx);
                    }
                }
                MsfDeltaOperation::Clone { tracks } => {
                    // draft-ietf-moq-msf-01 §5.1.3 (Is Complete): add と同様、clone による
                    // 新規トラック追加も拒否する。
                    if self.is_complete {
                        return Err(MessageError::InvalidCatalog(
                            "delta clone: catalog is complete; no new tracks may be added"
                                .to_string(),
                        ));
                    }
                    for clone in tracks {
                        let parent_ns = clone.parent_namespace.as_deref().or(catalog_namespace);
                        let idx = self
                            .find_track_index(parent_ns, &clone.parent_name, catalog_namespace)
                            .ok_or_else(|| {
                                MessageError::InvalidCatalog(format!(
                                    "delta clone: parent track '{}' not found",
                                    clone.parent_name
                                ))
                            })?;
                        let parent = self.tracks[idx].clone();
                        let new_track = clone.clone().into_track(&parent)?;
                        let new_ns = new_track.namespace.as_deref().or(catalog_namespace);
                        if self
                            .find_track_index(new_ns, &new_track.name, catalog_namespace)
                            .is_some()
                        {
                            return Err(MessageError::InvalidCatalog(format!(
                                "delta clone: track '{}' already exists",
                                new_track.name
                            )));
                        }
                        self.tracks.push(new_track);
                    }
                }
            }
        }
        // デルタ更新の generatedAt は適用後のカタログの生成時刻として引き継ぐ
        if let Some(generated_at) = delta.generated_at {
            self.generated_at = Some(generated_at);
        }
        self.validate_after_delta(catalog_namespace)
    }

    /// namespace / name が一致するトラックの index を返す
    ///
    /// `namespace` が `None` の場合はカタログの namespace を継承したものとして比較する
    /// (draft-ietf-moq-msf-01 §5.2.2 (Track namespace))。
    fn find_track_index(
        &self,
        namespace: Option<&str>,
        name: &str,
        catalog_namespace: Option<&str>,
    ) -> Option<usize> {
        self.tracks.iter().position(|t| {
            t.name == name && t.namespace.as_deref().or(catalog_namespace) == namespace
        })
    }

    /// デルタ適用後のカタログが満たすべき制約を再検証する
    fn validate_after_delta(&self, catalog_namespace: Option<&str>) -> Result<(), MessageError> {
        // draft-ietf-moq-msf-01 §5.2.3 (Track name): track name は namespace ごとに一意。
        // "Within the catalog" のため tracks と publishTracks をまたいで検査する。
        // namespace 省略時はカタログの namespace を継承したものとして解決して比較する。
        let mut seen: hashbrown::HashSet<(Option<&str>, &str)> = hashbrown::HashSet::new();
        for t in self.tracks.iter().chain(self.publish_tracks.iter()) {
            let ns = t.namespace.as_deref().or(catalog_namespace);
            if !seen.insert((ns, t.name.as_str())) {
                return Err(MessageError::InvalidCatalog(format!(
                    "duplicate track name '{}' in namespace '{}'",
                    t.name,
                    ns.unwrap_or("(inherited)")
                )));
            }
        }
        // draft-ietf-moq-msf-01 §5.2.13 (Initialization reference): 適用後の tracks /
        // publishTracks の initRef が initDataList の id を指すこと。
        validate_init_refs(&self.tracks, &self.publish_tracks, &self.init_data_list)?;
        // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers):
        // 同一 renderGroup / altGroup 内の isLive=true トラックの値は一致
        validate_group_target_latency(&self.tracks, "renderGroup", |t| t.render_group)?;
        validate_group_target_latency(&self.tracks, "altGroup", |t| t.alt_group)?;
        validate_group_buffers(&self.tracks, "renderGroup", |t| t.render_group)?;
        validate_group_buffers(&self.tracks, "altGroup", |t| t.alt_group)?;
        Ok(())
    }
}

/// デルタ更新の個別操作 (draft-ietf-moq-msf-01 §5.1.6 (Delta update))
///
/// draft-ietf-moq-msf-01 §5.1.6 (Delta update): 各 operation object は "op" フィールドと
/// "tracks" フィールドを持つ。
/// この仕様は将来変更される可能性がある。
#[derive(Debug, Clone, PartialEq)]
pub enum MsfDeltaOperation {
    /// トラック追加 ("add")
    Add {
        /// 追加するトラック一覧
        tracks: Vec<MsfTrack>,
    },
    /// トラック削除 ("remove")
    Remove {
        /// 削除するトラック参照一覧
        tracks: Vec<MsfRemoveTrack>,
    },
    /// トラッククローン ("clone")
    Clone {
        /// クローンするトラック定義一覧
        tracks: Vec<MsfCloneTrack>,
    },
}

/// デルタ更新 (draft-ietf-moq-msf-01 §5.1.6 (Delta update))
///
/// draft-ietf-moq-msf-01 §5.1.6 (Delta update): 操作は配列順に適用される。
/// この仕様は将来変更される可能性がある。
#[derive(Debug, Clone, PartialEq)]
pub struct MsfDeltaUpdate {
    /// カタログ生成時刻 ms
    pub generated_at: Option<u64>,
    /// 操作リスト (配列順)
    pub operations: Vec<MsfDeltaOperation>,
}

impl DisplayJson for MsfDeltaUpdate {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        // draft-ietf-moq-msf-01 §5.1.6 (Delta update): deltaUpdate は operation object の配列。
        // 操作は operations 配列順に 1 つの operation object ずつ出力する。
        // この仕様は将来変更される可能性がある。
        f.object(|f| {
            f.member(
                "deltaUpdate",
                nojson::array(|f| {
                    for op in &self.operations {
                        f.element(op)?;
                    }
                    Ok(())
                }),
            )?;
            if let Some(ga) = self.generated_at {
                f.member("generatedAt", ga)?;
            }
            Ok(())
        })
    }
}

impl DisplayJson for MsfDeltaOperation {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            match self {
                Self::Add { tracks } => {
                    f.member("op", "add")?;
                    f.member(
                        "tracks",
                        nojson::array(|f| {
                            for t in tracks {
                                f.element(t)?;
                            }
                            Ok(())
                        }),
                    )?;
                }
                Self::Remove { tracks } => {
                    f.member("op", "remove")?;
                    f.member(
                        "tracks",
                        nojson::array(|f| {
                            for t in tracks {
                                f.element(t)?;
                            }
                            Ok(())
                        }),
                    )?;
                }
                Self::Clone { tracks } => {
                    f.member("op", "clone")?;
                    f.member(
                        "tracks",
                        nojson::array(|f| {
                            for t in tracks {
                                f.element(t)?;
                            }
                            Ok(())
                        }),
                    )?;
                }
            }
            Ok(())
        })
    }
}

/// MSF カタログドキュメント
#[derive(Debug, Clone, PartialEq)]
pub enum MsfCatalogDocument {
    /// Full カタログ
    Full(MsfCatalog),
    /// デルタ更新
    Delta(MsfDeltaUpdate),
}

impl DisplayJson for MsfCatalogDocument {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        match self {
            Self::Full(c) => c.fmt(f),
            Self::Delta(d) => d.fmt(f),
        }
    }
}

impl MsfCatalogDocument {
    /// 全 authInfo エントリを列挙する (encode 前検証用)
    fn auth_infos(&self) -> Vec<&MsfAuthInfo> {
        let mut infos = Vec::new();
        match self {
            Self::Full(catalog) => {
                // publishTracks も tracks と同じ MsfTrack を出力するため検証対象に含める
                for track in catalog.tracks.iter().chain(catalog.publish_tracks.iter()) {
                    if let Some(auth_info) = track.auth_info.as_ref() {
                        infos.extend(auth_info.iter());
                    }
                }
            }
            Self::Delta(delta) => {
                for op in &delta.operations {
                    match op {
                        MsfDeltaOperation::Add { tracks } => {
                            for track in tracks {
                                if let Some(auth_info) = track.auth_info.as_ref() {
                                    infos.extend(auth_info.iter());
                                }
                            }
                        }
                        MsfDeltaOperation::Clone { tracks } => {
                            for track in tracks {
                                if let Some(auth_info) = track.auth_info.as_ref() {
                                    infos.extend(auth_info.iter());
                                }
                            }
                        }
                        MsfDeltaOperation::Remove { .. } => {}
                    }
                }
            }
        }
        infos
    }

    /// カタログドキュメントを JSON バイト列にエンコードする
    ///
    /// `authInfo` の `value_raw` を事前検証するため `Result` を返す
    /// (`MsfEventTimeline::encode` と対称)。手組みの不正値 (非 UTF-8、
    /// 単独の JSON 値でない) は `InvalidCatalog` で拒否し、panic と
    /// 不正 JSON の出力を防ぐ。
    ///
    /// さらに `decode_full_catalog` / `decode_delta` と同等の MUST 検証を encode 前に行い、
    /// 自身が decode できないカタログを出力しないことを保証する。
    ///
    /// # Errors
    ///
    /// - `authInfo` の値が UTF-8 でない / 単独の JSON 値でない: `InvalidCatalog`
    /// - 完全カタログが MUST に違反する (一意性、initRef 参照、フィールド整合など): `InvalidCatalog`
    /// - デルタ更新が空の operations を持つなど MUST に違反する: `InvalidCatalog`
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        for info in self.auth_infos() {
            validate_auth_info_value_raw(&info.value_raw)?;
        }
        match self {
            Self::Full(catalog) => validate_full_catalog_for_encode(catalog)?,
            Self::Delta(delta) => validate_delta_for_encode(delta)?,
        }
        Ok(nojson::Json(self).to_string().into_bytes())
    }

    /// JSON バイト列からカタログドキュメントをデコードする
    ///
    /// # Errors
    ///
    /// - UTF-8 でない: `InvalidCatalog`
    /// - JSON パースエラー: `InvalidCatalog`
    /// - 必須フィールドの欠如: `InvalidCatalog`
    /// - 未対応バージョン: `InvalidCatalogVersion`
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        let text = core::str::from_utf8(buf)
            .map_err(|_| MessageError::InvalidCatalog("catalog is not valid UTF-8".to_string()))?;
        let raw = nojson::RawJson::parse(text)?;
        let val = raw.value();

        // deltaUpdate が存在し配列なら Delta、存在しないか配列以外なら Full として扱う
        let delta_member = val.to_member("deltaUpdate")?.optional();
        if let Some(delta_v) = delta_member {
            // draft-ietf-moq-msf-01 §5.1.6 (Delta update): deltaUpdate は operation object の配列
            // 配列以外の型は InvalidCatalog とする
            let _ = delta_v.to_array().map_err(|_| {
                MessageError::InvalidCatalog("deltaUpdate MUST be an array".to_string())
            })?;
            return Ok(Self::Delta(decode_delta(val)?));
        }

        let version_v = val.to_member("version")?.required()?;
        let version: String = version_v
            .try_into()
            .map_err(|_| MessageError::InvalidCatalog("version MUST be a string".to_string()))?;
        if version != MSF_VERSION {
            return Err(MessageError::InvalidCatalogVersion(version));
        }
        Ok(Self::Full(decode_full_catalog(val, version)?))
    }
}

// ─── タイムライン型定義 ───────────────────────────────────────────────────────

/// メディアタイムラインエントリ (draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload))
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MsfMediaTimelineEntry {
    /// メディア PTS ms
    pub pts_ms: u64,
    /// MOQT Group ID
    pub group_id: u64,
    /// MOQT Object ID
    pub object_id: u64,
    /// ウォールクロック ms (不明な場合は 0)
    pub wallclock_ms: u64,
}

/// メディアタイムライントラックペイロード (draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload))
///
/// フォーマット: `[[pts_ms, [group_id, object_id], wallclock_ms], ...]`
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MsfMediaTimeline(
    /// タイムラインのエントリ列 (JSON 配列順)
    pub Vec<MsfMediaTimelineEntry>,
);

impl DisplayJson for MsfMediaTimeline {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.array(|f| {
            for e in &self.0 {
                let g = e.group_id;
                let o = e.object_id;
                f.element(nojson::array(|f| {
                    f.element(e.pts_ms)?;
                    f.element(nojson::array(|f| {
                        f.element(g)?;
                        f.element(o)
                    }))?;
                    f.element(e.wallclock_ms)
                }))?;
            }
            Ok(())
        })
    }
}

impl MsfMediaTimeline {
    /// 空のメディアタイムラインを作成する
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// JSON バイト列にエンコードする
    ///
    /// 全フィールドが型付きで失敗する入力が存在しないため Result を返さない
    /// (`MsfEventTimeline::encode` は生バイト列 `data_raw` を持つため Result を返す)。
    pub fn encode(&self) -> Vec<u8> {
        nojson::Json(self).to_string().into_bytes()
    }

    /// JSON バイト列からデコードする
    ///
    /// # Errors
    ///
    /// - JSON パースエラー: `InvalidCatalog`
    /// - フォーマット不正: `InvalidCatalog`
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        let text = core::str::from_utf8(buf).map_err(|_| {
            MessageError::InvalidCatalog("media timeline is not valid UTF-8".to_string())
        })?;
        let raw = nojson::RawJson::parse(text)?;
        let val = raw.value();
        let entries = val
            .to_array()?
            .map(decode_media_entry)
            .collect::<Result<_, _>>()?;
        Ok(Self(entries))
    }
}

/// イベントタイムラインのインデックス参照 (draft-ietf-moq-msf-01 §8.1 (Event Timeline data format))
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MsfEventIndex {
    /// 'l': MOQT Location [group_id, object_id]
    Location(u64, u64),
    /// 't': ウォールクロック ms
    WallclockMs(u64),
    /// 'm': メディア PTS ms
    MediaPtsMs(u64),
}

/// イベントタイムラインエントリ (draft-ietf-moq-msf-01 §8.1 (Event Timeline data format))
#[derive(Debug, Clone, PartialEq)]
pub struct MsfEventTimelineEntry {
    /// インデックス参照
    pub index: MsfEventIndex,
    /// データフィールドの JSON バイト列 (アプリケーション定義)
    pub data_raw: Vec<u8>,
}

impl DisplayJson for MsfEventTimelineEntry {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.object(|f| {
            match &self.index {
                MsfEventIndex::Location(g, o) => {
                    let g = *g;
                    let o = *o;
                    f.member(
                        "l",
                        nojson::array(|f| {
                            f.element(g)?;
                            f.element(o)
                        }),
                    )?;
                }
                MsfEventIndex::WallclockMs(t) => {
                    f.member("t", *t)?;
                }
                MsfEventIndex::MediaPtsMs(m) => {
                    f.member("m", *m)?;
                }
            }
            f.member("data", RawJsonBytes(&self.data_raw))
        })
    }
}

/// イベントタイムライントラックペイロード (draft-ietf-moq-msf-01 §8.1 (Event Timeline data format))
///
/// フォーマット: `[{"t"/"l"/"m": ..., "data": ...}, ...]`
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MsfEventTimeline(
    /// タイムラインのエントリ列 (JSON 配列順)
    pub Vec<MsfEventTimelineEntry>,
);

impl DisplayJson for MsfEventTimeline {
    fn fmt(&self, f: &mut JsonFormatter<'_, '_>) -> core::fmt::Result {
        f.array(|f| f.elements(self.0.iter()))
    }
}

impl MsfEventTimeline {
    /// 空のイベントタイムラインを作成する
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// JSON バイト列にエンコードする
    ///
    /// # Errors
    ///
    /// - `data_raw` が UTF-8 でない: `InvalidCatalog`
    /// - `data_raw` 全体が単一の JSON object としてパース可能でない (trailing 文字を含む): `InvalidCatalog`
    pub fn encode(&self) -> Result<Vec<u8>, MessageError> {
        // draft-ietf-moq-msf-01 §8.1 (Event Timeline data format): "An event timeline track is a JSON [JSON] document."
        // この仕様は将来変更される可能性がある。
        // data_raw は利用者が任意に構築できる pub フィールドのため、encode 前に検証して
        // panic 経路と不正な JSON の生成を防ぐ (検証の詳細は validate_event_data_raw を参照)。
        for entry in &self.0 {
            validate_event_data_raw(&entry.data_raw)?;
        }
        Ok(nojson::Json(self).to_string().into_bytes())
    }

    /// JSON バイト列からデコードする
    ///
    /// # Errors
    ///
    /// - JSON パースエラー: `InvalidCatalog`
    /// - 必須フィールドの欠如: `InvalidCatalog`
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        let text = core::str::from_utf8(buf).map_err(|_| {
            MessageError::InvalidCatalog("event timeline is not valid UTF-8".to_string())
        })?;
        let raw = nojson::RawJson::parse(text)?;
        let val = raw.value();
        let entries = val
            .to_array()?
            .map(decode_event_entry)
            .collect::<Result<_, _>>()?;
        Ok(Self(entries))
    }
}

// ─── 内部デコード・検証ヘルパー ────────────────────────────────────────────────────

/// 同一グループ内で targetLatency が一致することを検証する
///
/// draft-ietf-moq-msf-01 §5.2.8 (Target latency): isLive=false のトラックでは targetLatency は無視されるため、
/// 同一 group 内の isLive=true トラック間で一致することのみを検証する。
fn validate_group_target_latency(
    tracks: &[MsfTrack],
    group_name: &str,
    get_group: impl Fn(&MsfTrack) -> Option<u64>,
) -> Result<(), MessageError> {
    let mut seen: hashbrown::HashMap<u64, Option<u64>> = hashbrown::HashMap::new();
    for t in tracks {
        // isLive=false のトラックでは targetLatency は無視されるため比較対象外とする
        if !t.is_live {
            continue;
        }
        if let Some(gid) = get_group(t) {
            match seen.entry(gid) {
                hashbrown::hash_map::Entry::Vacant(e) => {
                    e.insert(t.target_latency);
                }
                hashbrown::hash_map::Entry::Occupied(e) => {
                    if *e.get() != t.target_latency {
                        return Err(MessageError::InvalidCatalog(format!(
                            "tracks in {group_name} {gid} have different targetLatency values"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// 同一グループ内で buffers が一致することを検証する
///
/// draft-ietf-moq-msf-01 §5.2.9 (Buffers): isLive=false のトラックでは buffers は無視されるため、
/// 同一 group 内の isLive=true トラック間で一致することのみを検証する。
fn validate_group_buffers(
    tracks: &[MsfTrack],
    group_name: &str,
    get_group: impl Fn(&MsfTrack) -> Option<u64>,
) -> Result<(), MessageError> {
    let mut seen: hashbrown::HashMap<u64, Option<MsfBuffers>> = hashbrown::HashMap::new();
    for t in tracks {
        // isLive=false のトラックでは buffers は無視されるため比較対象外とする
        if !t.is_live {
            continue;
        }
        if let Some(gid) = get_group(t) {
            match seen.entry(gid) {
                hashbrown::hash_map::Entry::Vacant(e) => {
                    e.insert(t.buffers);
                }
                hashbrown::hash_map::Entry::Occupied(e) => {
                    if *e.get() != t.buffers {
                        return Err(MessageError::InvalidCatalog(format!(
                            "tracks in {group_name} {gid} have different buffers values"
                        )));
                    }
                }
            }
        }
    }
    Ok(())
}

/// role が video / audio / audiodescription のトラックに必須のフィールドを検証する
///
/// draft-ietf-moq-msf-01 §5.2.18 (Codec) / §5.2.22 (Maximum Bitrate) /
/// §5.2.28 (Audio sample rate) / §5.2.29 (Channel configuration)。
/// role が省略されたトラックは「inherent codec を持つ」と機械的に判定できないため、
/// role が明示されたトラックのみを対象とする。
/// この仕様は将来変更される可能性がある。
fn validate_media_track_fields(track: &MsfTrack) -> Result<(), MessageError> {
    let is_audio = matches!(
        track.role.as_deref(),
        Some("audio") | Some("audiodescription")
    );
    let is_video = matches!(track.role.as_deref(), Some("video"));
    if !is_audio && !is_video {
        return Ok(());
    }
    if track.codec.is_none() {
        return Err(MessageError::InvalidCatalog(format!(
            "track '{}' with role '{}' MUST specify codec",
            track.name,
            track.role.as_deref().unwrap_or("")
        )));
    }
    if track.bitrate.is_none() {
        return Err(MessageError::InvalidCatalog(format!(
            "track '{}' with role '{}' MUST specify bitrate",
            track.name,
            track.role.as_deref().unwrap_or("")
        )));
    }
    if is_audio {
        if track.samplerate.is_none() {
            return Err(MessageError::InvalidCatalog(format!(
                "audio track '{}' MUST specify samplerate",
                track.name
            )));
        }
        if track.channel_config.is_none() {
            return Err(MessageError::InvalidCatalog(format!(
                "audio track '{}' MUST specify channelConfig",
                track.name
            )));
        }
    }
    Ok(())
}

/// initDataList の id が catalog 内で一意であることを検証する
///
/// draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List): id は catalog 内で一意。
fn validate_init_data_list_unique(init_data_list: &[MsfInitData]) -> Result<(), MessageError> {
    let mut seen_ids = hashbrown::HashSet::new();
    for entry in init_data_list {
        if !seen_ids.insert(entry.id.as_str()) {
            return Err(MessageError::InvalidCatalog(format!(
                "duplicate initDataList id '{}'",
                entry.id
            )));
        }
    }
    Ok(())
}

/// tracks / publishTracks をまたいだ (namespace, name) の一意性を検証する
///
/// draft-ietf-moq-msf-01 §5.2.3 (Track name): "Within the catalog, track names MUST be
/// unique per namespace." のため、catalog 全体で検査する。
fn validate_track_name_uniqueness(
    tracks: &[MsfTrack],
    publish_tracks: &[MsfTrack],
) -> Result<(), MessageError> {
    let mut seen: hashbrown::HashSet<(Option<&str>, &str)> = hashbrown::HashSet::new();
    for t in tracks.iter().chain(publish_tracks.iter()) {
        if !seen.insert((t.namespace.as_deref(), t.name.as_str())) {
            return Err(MessageError::InvalidCatalog(format!(
                "duplicate track name '{}' in namespace '{}'",
                t.name,
                t.namespace.as_deref().unwrap_or("(inherited)")
            )));
        }
    }
    Ok(())
}

/// tracks / publishTracks の initRef が initDataList の id を指すことを検証する
///
/// draft-ietf-moq-msf-01 §5.2.13 (Initialization reference): initRef は initDataList の
/// id を指す。完全カタログ単体で参照整合性を検証する。
fn validate_init_refs(
    tracks: &[MsfTrack],
    publish_tracks: &[MsfTrack],
    init_data_list: &[MsfInitData],
) -> Result<(), MessageError> {
    for t in tracks.iter().chain(publish_tracks.iter()) {
        if let Some(ref init_ref) = t.init_ref
            && !init_data_list.iter().any(|d| &d.id == init_ref)
        {
            return Err(MessageError::InvalidCatalog(format!(
                "initRef '{init_ref}' does not match any initDataList id"
            )));
        }
    }
    Ok(())
}

/// 完全カタログのトラック 1 件が満たすべき MUST を検証する (encode 前検証用)
///
/// `decode_track` の構造体ベース検証と同等の規則を、手組みの `MsfTrack` に対しても
/// 適用する。JSON キーの存在で判定する `targetLatency` / `buffers` の共存は、
/// 構造体では両フィールドが `Some` であることで判定する。
///
/// - draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)
/// - §5.2.8 (Target latency) / §5.2.9 (Buffers)
/// - §5.2.13 (Initialization reference) は呼び出し側で catalog 全体を検査する
/// - §5.2.32 (Language) / §5.2.33 (Parent name) / §5.2.35 (Track duration)
/// - §5.2.39 (Cipher suite) / §4.3.3 (Recommended encryption scheme)
/// - §7.2 (Media Timeline Catalog requirements) / §8.2 (Event Timeline Catalog requirements)
/// - §5.2.18 (Codec) / §5.2.22 (Maximum Bitrate) / §5.2.28 (Audio sample rate) /
///   §5.2.29 (Channel configuration)
fn validate_full_track(track: &MsfTrack) -> Result<(), MessageError> {
    // draft-ietf-moq-msf-01 §5.2.33 (Parent name): clone 操作内でのみ許可される
    if track.parent_name.is_some() {
        return Err(MessageError::InvalidCatalog(
            "parentName MUST only be included inside a clone operation in a delta update"
                .to_string(),
        ));
    }

    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type): eventtimeline なら eventType 必須、それ以外では禁止
    let is_event_timeline = matches!(track.packaging, MsfPackaging::EventTimeline);
    if is_event_timeline && track.event_type.is_none() {
        return Err(MessageError::InvalidCatalog(
            "eventType is required when packaging is 'eventtimeline'".to_string(),
        ));
    }
    if !is_event_timeline && track.event_type.is_some() {
        return Err(MessageError::InvalidCatalog(
            "eventType MUST NOT be used when packaging is not 'eventtimeline'".to_string(),
        ));
    }

    // draft-ietf-moq-msf-01 §5.2.39 (Cipher suite) / §4.3.3 (Recommended encryption scheme)
    if track.encryption_scheme.is_some() && track.cipher_suite.is_none() {
        return Err(MessageError::InvalidCatalog(
            "cipherSuite MUST be present when encryptionScheme is specified".to_string(),
        ));
    }
    if track.encryption_scheme.as_deref() == Some("moq-secure-objects")
        && (track.key_id.is_none() || track.track_base_key.is_none())
    {
        return Err(MessageError::InvalidCatalog(
            "keyId and trackBaseKey MUST be present with moq-secure-objects".to_string(),
        ));
    }

    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers): 共存禁止
    if track.target_latency.is_some() && track.buffers.is_some() {
        return Err(MessageError::InvalidCatalog(
            "targetLatency and buffers MUST NOT be present together".to_string(),
        ));
    }

    // draft-ietf-moq-msf-01 §5.2.35 (Track duration): isLive=true なら禁止
    if track.is_live && track.track_duration.is_some() {
        return Err(MessageError::InvalidCatalog(
            "trackDuration MUST NOT be included when isLive is true".to_string(),
        ));
    }

    // draft-ietf-moq-msf-01 §7.2 (Media Timeline Catalog requirements) / §8.2 (Event Timeline Catalog requirements)
    let is_media_timeline = matches!(track.packaging, MsfPackaging::MediaTimeline);
    if is_media_timeline {
        if track.depends.is_empty() {
            return Err(MessageError::InvalidCatalog(
                "mediatimeline track MUST have a 'depends' attribute".to_string(),
            ));
        }
        if track.mime_type.as_deref() != Some("application/json") {
            return Err(MessageError::InvalidCatalog(
                "mediatimeline track mimeType MUST be 'application/json'".to_string(),
            ));
        }
    }
    if is_event_timeline {
        if track.depends.is_empty() {
            return Err(MessageError::InvalidCatalog(
                "eventtimeline track MUST have a 'depends' attribute".to_string(),
            ));
        }
        if track.mime_type.as_deref() != Some("application/json") {
            return Err(MessageError::InvalidCatalog(
                "eventtimeline track mimeType MUST be 'application/json'".to_string(),
            ));
        }
    }

    // draft-ietf-moq-msf-01 §5.2.32 (Language): BCP 47 言語タグ
    if let Some(ref lang) = track.lang {
        validate_lang_tag(lang)?;
    }

    validate_media_track_fields(track)
}

/// Delta の clone トラック断片が満たすべき MUST を検証する (encode 前検証用)
///
/// `decode_clone_track` の検証と同等の規則を、手組みの `MsfCloneTrack` に適用する。
/// clone は親から属性を継承し得るため、欠如側の必須検査は行わない。
///
/// - draft-ietf-moq-msf-01 §5.2.5 (Event timeline type)
/// - §5.2.8 (Target latency) / §5.2.9 (Buffers)
/// - §5.2.32 (Language) / §5.2.35 (Track duration)
fn validate_clone_track_fragment(track: &MsfCloneTrack) -> Result<(), MessageError> {
    // packaging を明示して非 eventtimeline とした場合は eventType を禁止する
    // (packaging 省略 + eventType ありは親からの継承で正当化され得るため受理する)
    if let Some(ref pkg) = track.packaging {
        let is_event_timeline = matches!(pkg, MsfPackaging::EventTimeline);
        if !is_event_timeline && track.event_type.is_some() {
            return Err(MessageError::InvalidCatalog(
                "eventType MUST NOT be used when packaging is not 'eventtimeline'".to_string(),
            ));
        }
    }

    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers): 共存禁止
    if track.target_latency.is_some() && track.buffers.is_some() {
        return Err(MessageError::InvalidCatalog(
            "targetLatency and buffers MUST NOT be present together".to_string(),
        ));
    }

    // draft-ietf-moq-msf-01 §5.2.35 (Track duration): isLive=true なら禁止
    if track.is_live == Some(true) && track.track_duration.is_some() {
        return Err(MessageError::InvalidCatalog(
            "trackDuration MUST NOT be included when isLive is true".to_string(),
        ));
    }

    // draft-ietf-moq-msf-01 §5.2.32 (Language): BCP 47 言語タグ
    if let Some(ref lang) = track.lang {
        validate_lang_tag(lang)?;
    }
    Ok(())
}

/// 完全カタログを encode する前の MUST 検証
///
/// `decode_full_catalog` が行う検証と同等の規則を、手組みの `MsfCatalog` にも適用する。
fn validate_full_catalog_for_encode(catalog: &MsfCatalog) -> Result<(), MessageError> {
    validate_init_data_list_unique(&catalog.init_data_list)?;
    validate_init_refs(
        &catalog.tracks,
        &catalog.publish_tracks,
        &catalog.init_data_list,
    )?;
    validate_track_name_uniqueness(&catalog.tracks, &catalog.publish_tracks)?;
    for track in catalog.tracks.iter().chain(catalog.publish_tracks.iter()) {
        validate_full_track(track)?;
    }
    // tracks と publishTracks は方向が異なるため group 検証は分離する
    // (draft-ietf-moq-msf-01 §5.1.5 の reverse direction による本実装の解釈)
    validate_group_target_latency(&catalog.tracks, "renderGroup", |t| t.render_group)?;
    validate_group_target_latency(&catalog.tracks, "altGroup", |t| t.alt_group)?;
    validate_group_target_latency(&catalog.publish_tracks, "renderGroup", |t| t.render_group)?;
    validate_group_target_latency(&catalog.publish_tracks, "altGroup", |t| t.alt_group)?;
    validate_group_buffers(&catalog.tracks, "renderGroup", |t| t.render_group)?;
    validate_group_buffers(&catalog.tracks, "altGroup", |t| t.alt_group)?;
    validate_group_buffers(&catalog.publish_tracks, "renderGroup", |t| t.render_group)?;
    validate_group_buffers(&catalog.publish_tracks, "altGroup", |t| t.alt_group)?;
    Ok(())
}

/// デルタ更新を encode する前の MUST 検証
///
/// `decode_delta` / `decode_delta_operation` が行う検証と同等の規則を、手組みの
/// `MsfDeltaUpdate` にも適用する。
fn validate_delta_for_encode(delta: &MsfDeltaUpdate) -> Result<(), MessageError> {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): 少なくとも 1 つの操作が必要
    if delta.operations.is_empty() {
        return Err(MessageError::InvalidCatalog(
            "delta update MUST contain at least one operation".to_string(),
        ));
    }
    for op in &delta.operations {
        match op {
            MsfDeltaOperation::Add { tracks } => {
                for track in tracks {
                    validate_full_track(track)?;
                }
            }
            MsfDeltaOperation::Clone { tracks } => {
                for track in tracks {
                    validate_clone_track_fragment(track)?;
                }
            }
            MsfDeltaOperation::Remove { .. } => {}
        }
    }
    Ok(())
}

fn decode_full_catalog(
    val: nojson::RawJsonValue<'_, '_>,
    version: String,
) -> Result<MsfCatalog, MessageError> {
    let generated_at = val.to_member("generatedAt")?.map(parse_u64)?;
    // draft-ietf-moq-msf-01 §5.1.3 (Is Complete): isComplete が FALSE ならフィールド自体を含めてはならない
    // この仕様は将来変更される可能性がある。
    let is_complete = match val.to_member("isComplete")?.optional() {
        Some(v) => {
            let b: bool = v.try_into()?;
            if !b {
                return Err(MessageError::InvalidCatalog(
                    "isComplete MUST NOT be included if it is FALSE".to_string(),
                ));
            }
            true
        }
        None => false,
    };
    let tracks_v = val.to_member("tracks")?.required()?;
    let tracks = tracks_v
        .to_array()?
        .map(|v| decode_track(v))
        .collect::<Result<Vec<_>, _>>()?;

    // draft-ietf-moq-msf-01 §5.1.5 (Publish tracks): 欠如時は空とする
    // (JSON キーの出現順には依存しない。順序検証は行わない)
    let publish_tracks = match val.to_member("publishTracks")?.optional() {
        None => Vec::new(),
        Some(arr_v) => arr_v
            .to_array()?
            .map(|v| decode_track(v))
            .collect::<Result<Vec<_>, _>>()?,
    };

    // draft-ietf-moq-msf-01 §5.1.7 (Initialization Data List): initDataList は tracks 配列より後に出現しなければならない
    // nojson はキー出現順を提供しないため、順序検証は行わない。
    // この仕様は将来変更される可能性がある。
    let init_data_list = match val.to_member("initDataList")?.optional() {
        Some(v) => v
            .to_array()?
            .map(decode_init_data)
            .collect::<Result<Vec<_>, _>>()?,
        None => Vec::new(),
    };
    validate_init_data_list_unique(&init_data_list)?;

    // draft-ietf-moq-msf-01 §5.2.13 (Initialization reference): initRef は initDataList の
    // id を指す。完全カタログ単体で参照整合性を検証する (delta は initDataList を持たず、
    // 基底カタログの id を継承し得るためここでは検証しない)。
    validate_init_refs(&tracks, &publish_tracks, &init_data_list)?;

    // draft-ietf-moq-msf-01 §5.2.3 (Track name): track name は namespace ごとに一意でなければならない
    // この仕様は将来変更される可能性がある。
    //
    // 注意: ここではカタログトラックの namespace が不明なため、JSON 上の値での部分的なチェックのみ行う。
    // namespace 省略 (None) と明示指定が実質同じ namespace になるケースの検出はできない
    // (カタログトラックの namespace が判明するのはカタログトラック自体の受信後であり、
    // デコード単体では判定不能)。
    //
    // draft-ietf-moq-msf-01 §5.2.3 (Track name) の一意性は catalog 全体 ("Within the catalog") の規定のため、
    // tracks と publishTracks を通して検査する。
    validate_track_name_uniqueness(&tracks, &publish_tracks)?;

    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): 同一 renderGroup / altGroup 内の
    // isLive=true トラックの targetLatency は一致しなければならない (MUST)
    // この仕様は将来変更される可能性がある。
    validate_group_target_latency(&tracks, "renderGroup", |t| t.render_group)?;
    validate_group_target_latency(&tracks, "altGroup", |t| t.alt_group)?;
    // tracks と publishTracks は方向が異なるため group 検証は分離する
    // (draft-ietf-moq-msf-01 §5.1.5 の reverse direction による本実装の解釈。
    // §5.2.8 / §5.2.9 の All tracks の範囲は曖昧であり、この仕様は将来変更される可能性がある。
    // 重複検査のみ §5.2.3 の Within the catalog に従い catalog 通し)
    validate_group_target_latency(&publish_tracks, "renderGroup", |t| t.render_group)?;
    validate_group_target_latency(&publish_tracks, "altGroup", |t| t.alt_group)?;

    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 同一 renderGroup / altGroup 内の
    // isLive=true トラックの buffers は一致しなければならない (MUST)
    // この仕様は将来変更される可能性がある。
    validate_group_buffers(&tracks, "renderGroup", |t| t.render_group)?;
    validate_group_buffers(&tracks, "altGroup", |t| t.alt_group)?;
    validate_group_buffers(&publish_tracks, "renderGroup", |t| t.render_group)?;
    validate_group_buffers(&publish_tracks, "altGroup", |t| t.alt_group)?;

    Ok(MsfCatalog {
        version,
        generated_at,
        is_complete,
        tracks,
        publish_tracks,
        init_data_list,
    })
}

fn decode_delta(val: nojson::RawJsonValue<'_, '_>) -> Result<MsfDeltaUpdate, MessageError> {
    // draft-ietf-moq-msf-01 §5.3 (Delta updates): delta update に version や tracks が含まれてはいけない
    if val.to_member("version")?.optional().is_some() {
        return Err(MessageError::InvalidCatalog(
            "delta update MUST NOT contain 'version'".to_string(),
        ));
    }
    if val.to_member("tracks")?.optional().is_some() {
        return Err(MessageError::InvalidCatalog(
            "delta update MUST NOT contain 'tracks'".to_string(),
        ));
    }

    let generated_at = val.to_member("generatedAt")?.map(parse_u64)?;

    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): deltaUpdate は operation object の配列。
    // 操作は配列順に適用される。
    // この仕様は将来変更される可能性がある。
    let operations_v = val.to_member("deltaUpdate")?.required()?;
    let mut operations = Vec::new();
    for op_v in operations_v.to_array()? {
        operations.push(decode_delta_operation(op_v)?);
    }

    // draft-ietf-moq-msf-01 §5.3 (Delta updates): 少なくとも 1 つの操作が必要
    if operations.is_empty() {
        return Err(MessageError::InvalidCatalog(
            "delta update MUST contain at least one operation".to_string(),
        ));
    }

    Ok(MsfDeltaUpdate {
        generated_at,
        operations,
    })
}

fn decode_delta_operation(
    val: nojson::RawJsonValue<'_, '_>,
) -> Result<MsfDeltaOperation, MessageError> {
    let op: String = val.to_member("op")?.required()?.try_into()?;
    let tracks_v = val.to_member("tracks")?.required()?;
    match op.as_str() {
        "add" => {
            let tracks = tracks_v
                .to_array()?
                .map(decode_track)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(MsfDeltaOperation::Add { tracks })
        }
        "remove" => {
            let tracks = tracks_v
                .to_array()?
                .map(decode_remove_track)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(MsfDeltaOperation::Remove { tracks })
        }
        "clone" => {
            let tracks = tracks_v
                .to_array()?
                .map(decode_clone_track)
                .collect::<Result<Vec<_>, _>>()?;
            Ok(MsfDeltaOperation::Clone { tracks })
        }
        other => Err(MessageError::InvalidCatalog(format!(
            "unknown delta operation '{other}'"
        ))),
    }
}

/// track / cloneTracks 共通フィールドの抽出結果 (decode 専用の内部型)
///
/// `MsfTrack` と `MsfCloneTrack` は公開型としては別 (必須/省略の意味が異なる) だが、
/// ワイヤ上のフィールド一覧・JSON キー・値の読み方は同一のため、抽出は 1 関数
/// (`decode_common_track_fields`) に集約する。片方への修正漏れを防ぐための集約であり、
/// 必須可否・継承可否などの妥当性規則は呼び出し側 (decode_track / decode_clone_track)
/// が担う。encode 側は機械的な member 書き出しであり、roundtrip テストで drift を検出する。
#[derive(Debug, Default)]
struct CommonTrackFields {
    namespace: Option<String>,
    event_type: Option<String>,
    role: Option<String>,
    target_latency: Option<u64>,
    buffers: Option<MsfBuffers>,
    label: Option<String>,
    render_group: Option<u64>,
    alt_group: Option<u64>,
    init_ref: Option<String>,
    template: Option<MsfTemplate>,
    temporal_id: Option<u64>,
    spatial_id: Option<u64>,
    codec: Option<String>,
    mime_type: Option<String>,
    framerate: Option<f64>,
    timescale: Option<u64>,
    bitrate: Option<u64>,
    avg_bitrate: Option<u64>,
    max_gop_duration: Option<u64>,
    max_group_duration: Option<u64>,
    width: Option<u64>,
    height: Option<u64>,
    samplerate: Option<u64>,
    channel_config: Option<String>,
    display_width: Option<u64>,
    display_height: Option<u64>,
    lang: Option<String>,
    track_duration: Option<u64>,
    connection_uri: Option<String>,
    token: Option<String>,
    encryption_scheme: Option<String>,
    cipher_suite: Option<String>,
    key_id: Option<String>,
    track_base_key: Option<String>,
    auth_info: Option<Vec<MsfAuthInfo>>,
    depends: Option<Vec<String>>,
    accessibility: Option<Vec<MsfAccessibility>>,
    parent_name: Option<String>,
    parent_namespace: Option<String>,
}

/// track / cloneTracks 共通フィールドを抽出する
///
/// lang タグ検証まで含む (両者で同一)。必須可否は呼び出し側で判定する。
fn decode_common_track_fields(
    val: nojson::RawJsonValue<'_, '_>,
) -> Result<CommonTrackFields, MessageError> {
    let fields = CommonTrackFields {
        namespace: val.to_member("namespace")?.map(|v| v.try_into())?,
        event_type: val.to_member("eventType")?.map(|v| v.try_into())?,
        role: val.to_member("role")?.map(|v| v.try_into())?,
        target_latency: val.to_member("targetLatency")?.map(parse_u64)?,
        buffers: match val.to_member("buffers")?.optional() {
            Some(v) => Some(decode_buffers(v)?),
            None => None,
        },
        label: val.to_member("label")?.map(|v| v.try_into())?,
        render_group: val.to_member("renderGroup")?.map(parse_u64)?,
        alt_group: val.to_member("altGroup")?.map(parse_u64)?,
        init_ref: val.to_member("initRef")?.map(|v| v.try_into())?,
        template: match val.to_member("template")?.optional() {
            Some(v) => Some(decode_template(v)?),
            None => None,
        },
        temporal_id: val.to_member("temporalId")?.map(parse_u64)?,
        spatial_id: val.to_member("spatialId")?.map(parse_u64)?,
        codec: val.to_member("codec")?.map(|v| v.try_into())?,
        mime_type: val.to_member("mimeType")?.map(|v| v.try_into())?,
        framerate: val.to_member("framerate")?.map(parse_finite_f64)?,
        timescale: val.to_member("timescale")?.map(parse_u64)?,
        bitrate: val.to_member("bitrate")?.map(parse_u64)?,
        avg_bitrate: val.to_member("avgBitrate")?.map(parse_u64)?,
        max_gop_duration: val.to_member("maxGopDuration")?.map(parse_u64)?,
        max_group_duration: val.to_member("maxGroupDuration")?.map(parse_u64)?,
        width: val.to_member("width")?.map(parse_u64)?,
        height: val.to_member("height")?.map(parse_u64)?,
        samplerate: val.to_member("samplerate")?.map(parse_u64)?,
        channel_config: val.to_member("channelConfig")?.map(|v| v.try_into())?,
        display_width: val.to_member("displayWidth")?.map(parse_u64)?,
        display_height: val.to_member("displayHeight")?.map(parse_u64)?,
        lang: val.to_member("lang")?.map(|v| v.try_into())?,
        track_duration: val.to_member("trackDuration")?.map(parse_u64)?,
        // キー綴りは §5.2.36 / §5.2.37 に定義がなく §5.6.16 の例に準拠する。
        // URI 形式検証は行わない (§5.2.36 の妥当性 MUST を含め、接続確立時の振る舞いは
        // catalog decode の責務外とし transport 層に委ねる)。
        connection_uri: val.to_member("connectionUri")?.map(|v| v.try_into())?,
        token: val.to_member("token")?.map(|v| v.try_into())?,
        encryption_scheme: val.to_member("encryptionScheme")?.map(|v| v.try_into())?,
        cipher_suite: val.to_member("cipherSuite")?.map(|v| v.try_into())?,
        key_id: val.to_member("keyId")?.map(|v| v.try_into())?,
        track_base_key: val.to_member("trackBaseKey")?.map(|v| v.try_into())?,
        auth_info: match val.to_member("authInfo")?.optional() {
            None => None,
            Some(obj_v) => Some(decode_auth_info(obj_v)?),
        },
        depends: match val.to_member("depends")?.optional() {
            None => None,
            Some(arr_v) => Some(
                arr_v
                    .to_array()?
                    .map(|v| -> Result<String, MessageError> { v.try_into().map_err(Into::into) })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
        },
        accessibility: match val.to_member("accessibility")?.optional() {
            None => None,
            Some(arr_v) => Some(decode_accessibility(arr_v)?),
        },
        parent_name: val.to_member("parentName")?.map(|v| v.try_into())?,
        parent_namespace: val.to_member("parentNamespace")?.map(|v| v.try_into())?,
    };
    if let Some(ref lang) = fields.lang {
        validate_lang_tag(lang)?;
    }
    Ok(fields)
}

/// targetLatency と buffers の共存を拒否する (両者共通の presence 検査)
///
/// draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers)。
/// None 化より前に key 存在で判定する (パース済み値では None 化で情報が落ちるため)。
fn reject_coexisting_target_latency_buffers(
    val: nojson::RawJsonValue<'_, '_>,
) -> Result<(), MessageError> {
    if val.to_member("targetLatency")?.optional().is_some()
        && val.to_member("buffers")?.optional().is_some()
    {
        return Err(MessageError::InvalidCatalog(
            "targetLatency and buffers MUST NOT be present together".to_string(),
        ));
    }
    Ok(())
}

fn decode_track(val: nojson::RawJsonValue<'_, '_>) -> Result<MsfTrack, MessageError> {
    let name: String = val.to_member("name")?.required()?.try_into()?;
    let packaging_str: String = val.to_member("packaging")?.required()?.try_into()?;
    let packaging = MsfPackaging::from_packaging_str(&packaging_str)?;

    let f = decode_common_track_fields(val)?;
    let CommonTrackFields {
        namespace,
        event_type,
        role,
        target_latency,
        buffers,
        label,
        render_group,
        alt_group,
        init_ref,
        depends,
        template,
        temporal_id,
        spatial_id,
        codec,
        mime_type,
        framerate,
        timescale,
        bitrate,
        avg_bitrate,
        max_gop_duration,
        max_group_duration,
        width,
        height,
        samplerate,
        channel_config,
        display_width,
        display_height,
        lang,
        parent_name,
        parent_namespace,
        track_duration,
        connection_uri,
        token,
        encryption_scheme,
        cipher_suite,
        key_id,
        track_base_key,
        auth_info,
        accessibility,
    } = f;
    let depends = depends.unwrap_or_default();
    let accessibility = accessibility.unwrap_or_default();

    // draft-ietf-moq-msf-01 §5.2.5 (Event timeline type): eventtimeline なら eventType 必須、それ以外では禁止
    let is_event_timeline = matches!(packaging, MsfPackaging::EventTimeline);
    if is_event_timeline && event_type.is_none() {
        return Err(MessageError::InvalidCatalog(
            "eventType is required when packaging is 'eventtimeline'".to_string(),
        ));
    }
    if !is_event_timeline && event_type.is_some() {
        return Err(MessageError::InvalidCatalog(
            "eventType MUST NOT be used when packaging is not 'eventtimeline'".to_string(),
        ));
    }

    // draft-ietf-moq-msf-01 §5.2.7 (Is Live): isLive は必須フィールド
    // §5.6 の例は非規範例であり isLive を省略しているものがある。本実装は §5.2.7 に従い
    // required とするため、仕様例をそのままデコードすると reject される。テストで利用する際は isLive を補完すること
    // この仕様は将来変更される可能性がある。
    let is_live: bool = val.to_member("isLive")?.required()?.try_into()?;

    // ─── parentName / parentNamespace の配置制約 ─────────────
    // draft-ietf-moq-msf-01 §5.2.33 (Parent name) / §5.2.34 (Parent namespace):
    // 両者とも clone 操作 (delta update §5.1.6) 内でのみ許可される。
    // cloneTracks は decode_clone_track で処理されるため、ここでは常に禁止
    // (parentNamespace は本実装が decode_clone_track で理解するフィールドであり、
    // ここでの拒否は §5 の MUST ignore と衝突しない)。
    if parent_name.is_some() {
        return Err(MessageError::InvalidCatalog(
            "parentName MUST only be included inside a clone operation in a delta update"
                .to_string(),
        ));
    }
    if parent_namespace.is_some() {
        return Err(MessageError::InvalidCatalog(
            "parentNamespace MUST only be included inside a clone operation in a delta update"
                .to_string(),
        ));
    }

    // ─── 暗号化 signaling の必須組 ───
    // draft-ietf-moq-msf-01 §5.2.39: encryptionScheme 指定時は scheme によらず cipherSuite が必須。
    // draft-ietf-moq-msf-01 §4.3.3: moq-secure-objects 時は cipherSuite ・ keyId ・ trackBaseKey の 3 点が必須。
    // clone (decode_clone_track) では親未知のため欠如 reject を行わない (継承で補完され得る)。
    if encryption_scheme.is_some() && cipher_suite.is_none() {
        return Err(MessageError::InvalidCatalog(
            "cipherSuite MUST be present when encryptionScheme is specified".to_string(),
        ));
    }
    if encryption_scheme.as_deref() == Some("moq-secure-objects")
        && (key_id.is_none() || track_base_key.is_none())
    {
        return Err(MessageError::InvalidCatalog(
            "keyId and trackBaseKey MUST be present with moq-secure-objects".to_string(),
        ));
    }

    // ─── isLive と targetLatency / buffers / trackDuration の相互制約 ───
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): "This property MUST NOT be present if the
    // buffers Section 5.2.9 property is present within a track definition."
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): "This property MUST NOT be present if the target
    // latency Section 5.2.8 property is present within a track definition."
    // 共存は isLive の値によらず reject する (本実装では presence 違反を優先する選択とする)。
    // None 化より前に key 存在で判定する。
    reject_coexisting_target_latency_buffers(val)?;
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency): isLive=false なら targetLatency は無視される
    // draft-ietf-moq-msf-01 §5.2.9 (Buffers): isLive=false なら buffers は無視される
    let (target_latency, buffers) = if is_live {
        (target_latency, buffers)
    } else {
        (None, None)
    };
    // draft-ietf-moq-msf-01 §5.2.35 (Track duration): isLive=true なら trackDuration 禁止
    if is_live && track_duration.is_some() {
        return Err(MessageError::InvalidCatalog(
            "trackDuration MUST NOT be included when isLive is true".to_string(),
        ));
    }

    // ─── timeline track の必須属性 ────────────────
    let is_media_timeline = matches!(packaging, MsfPackaging::MediaTimeline);
    // draft-ietf-moq-msf-01 §7.2 (Media Timeline Catalog requirements): mediatimeline は depends 必須, mimeType=application/json 必須
    if is_media_timeline {
        if depends.is_empty() {
            return Err(MessageError::InvalidCatalog(
                "mediatimeline track MUST have a 'depends' attribute".to_string(),
            ));
        }
        if mime_type.as_deref() != Some("application/json") {
            return Err(MessageError::InvalidCatalog(
                "mediatimeline track mimeType MUST be 'application/json'".to_string(),
            ));
        }
    }
    // draft-ietf-moq-msf-01 §8.2 (Event Timeline Catalog requirements): eventtimeline は depends 必須, mimeType=application/json 必須
    if is_event_timeline {
        if depends.is_empty() {
            return Err(MessageError::InvalidCatalog(
                "eventtimeline track MUST have a 'depends' attribute".to_string(),
            ));
        }
        if mime_type.as_deref() != Some("application/json") {
            return Err(MessageError::InvalidCatalog(
                "eventtimeline track mimeType MUST be 'application/json'".to_string(),
            ));
        }
    }

    let track = MsfTrack {
        name,
        packaging,
        namespace,
        event_type,
        role,
        is_live,
        target_latency,
        buffers,
        label,
        render_group,
        alt_group,
        init_ref,
        depends,
        template,
        temporal_id,
        spatial_id,
        codec,
        mime_type,
        framerate,
        timescale,
        bitrate,
        avg_bitrate,
        max_gop_duration,
        max_group_duration,
        width,
        height,
        samplerate,
        channel_config,
        display_width,
        display_height,
        lang,
        parent_name,
        track_duration,
        connection_uri,
        token,
        encryption_scheme,
        cipher_suite,
        key_id,
        track_base_key,
        auth_info,
        accessibility,
    };
    validate_media_track_fields(&track)?;
    Ok(track)
}

fn decode_init_data(val: nojson::RawJsonValue<'_, '_>) -> Result<MsfInitData, MessageError> {
    let id: String = val.to_member("id")?.required()?.try_into()?;
    let kind_str: String = val.to_member("type")?.required()?.try_into()?;
    let kind = MsfInitDataKind::from_init_data_kind_str(&kind_str)?;
    let data: String = val.to_member("data")?.required()?.try_into()?;
    // 未知キーは無視する
    Ok(MsfInitData { id, kind, data })
}

fn decode_buffers(val: nojson::RawJsonValue<'_, '_>) -> Result<MsfBuffers, MessageError> {
    let mut target = None;
    let mut min = None;
    let mut max = None;
    for (key, value) in val.to_object()? {
        let key_str: &str = key.try_into()?;
        match key_str {
            "target" => target = Some(parse_u64(value)?),
            "min" => min = Some(parse_u64(value)?),
            "max" => max = Some(parse_u64(value)?),
            _ => {
                // draft-ietf-moq-msf-01 §5.2.9 (Buffers): 未知 key は無視する
            }
        }
    }
    Ok(MsfBuffers { target, min, max })
}

/// authInfo object をデコードする (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info))
///
/// scheme 名と値の生 JSON を順序どおりに保持する。非 object は reject する。
/// 空 object は空 `Vec` として受理する。`null` は非 object として reject する。
fn decode_auth_info(val: nojson::RawJsonValue<'_, '_>) -> Result<Vec<MsfAuthInfo>, MessageError> {
    let mut entries = Vec::new();
    for (key, value) in val.to_object()? {
        let key_str: &str = key.try_into()?;
        entries.push(MsfAuthInfo {
            scheme: key_str.to_string(),
            value_raw: value.as_raw_str().as_bytes().to_vec(),
        });
    }
    Ok(entries)
}

/// accessibility 配列をデコードする (draft-ietf-moq-msf-01 §5.2.44 (Accessibility))
///
/// 記述子内の `scheme` / `value` の欠如・型不正は reject する。
/// 配列自体の欠如時の扱いは呼び出し側が決める (Full は空、clone は `None` = 継承)。
fn decode_accessibility(
    val: nojson::RawJsonValue<'_, '_>,
) -> Result<Vec<MsfAccessibility>, MessageError> {
    let mut descs = Vec::new();
    for entry_v in val.to_array()? {
        let scheme: String = entry_v.to_member("scheme")?.required()?.try_into()?;
        let value: String = entry_v.to_member("value")?.required()?.try_into()?;
        descs.push(MsfAccessibility { scheme, value });
    }
    Ok(descs)
}

/// template 配列をデコードする (draft-ietf-moq-msf-01 §7.4.1 (Template Format))
///
/// 6 要素は必須であり指定順に現れる。入れ子の location 配列は要素数 2 とする。
/// 要素数・順序・型のいずれかに違反した場合は `InvalidCatalog` を返す。
fn decode_template(val: nojson::RawJsonValue<'_, '_>) -> Result<MsfTemplate, MessageError> {
    fn missing(what: &str) -> MessageError {
        MessageError::InvalidCatalog(format!("template is missing {what}"))
    }
    fn extra() -> MessageError {
        MessageError::InvalidCatalog("template must have exactly 6 elements".to_string())
    }
    // location 2 要素配列を (Group ID, Object ID) にデコードする
    fn decode_location_pair(
        val: nojson::RawJsonValue<'_, '_>,
        what: &str,
    ) -> Result<(u64, u64), MessageError> {
        let mut iter = val.to_array().map_err(|_| {
            MessageError::InvalidCatalog(format!("template {what} must be an array"))
        })?;
        let group_id = parse_u64(iter.next().ok_or_else(|| {
            MessageError::InvalidCatalog(format!("template {what} is missing group ID"))
        })?)?;
        let object_id = parse_u64(iter.next().ok_or_else(|| {
            MessageError::InvalidCatalog(format!("template {what} is missing object ID"))
        })?)?;
        if iter.next().is_some() {
            return Err(MessageError::InvalidCatalog(format!(
                "template {what} must have exactly 2 elements"
            )));
        }
        Ok((group_id, object_id))
    }
    let mut iter = val
        .to_array()
        .map_err(|_| MessageError::InvalidCatalog("template must be an array".to_string()))?;
    let start_media_time = parse_u64(iter.next().ok_or_else(|| missing("startMediaTime"))?)?;
    let delta_media_time = parse_u64(iter.next().ok_or_else(|| missing("deltaMediaTime"))?)?;
    let start_location = iter.next().ok_or_else(|| missing("startLocation"))?;
    let delta_location = iter.next().ok_or_else(|| missing("deltaLocation"))?;
    let start_wallclock = parse_u64(iter.next().ok_or_else(|| missing("startWallclock"))?)?;
    let delta_wallclock = parse_u64(iter.next().ok_or_else(|| missing("deltaWallclock"))?)?;
    if iter.next().is_some() {
        return Err(extra());
    }
    let (start_group_id, start_object_id) = decode_location_pair(start_location, "startLocation")?;
    let (delta_group_id, delta_object_id) = decode_location_pair(delta_location, "deltaLocation")?;
    Ok(MsfTemplate {
        start_media_time,
        delta_media_time,
        start_group_id,
        start_object_id,
        delta_group_id,
        delta_object_id,
        start_wallclock,
        delta_wallclock,
    })
}

fn decode_remove_track(val: nojson::RawJsonValue<'_, '_>) -> Result<MsfRemoveTrack, MessageError> {
    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): remove operation の track object は
    // name 必須、namespace 任意、それ以外は禁止
    // この仕様は将来変更される可能性がある。
    for (key, _) in val.to_object()? {
        let key_str: &str = key.try_into()?;
        if key_str != "name" && key_str != "namespace" {
            return Err(MessageError::InvalidCatalog(format!(
                "removeTracks entry contains forbidden field '{key_str}'"
            )));
        }
    }
    let name: String = val.to_member("name")?.required()?.try_into()?;
    let namespace: Option<String> = val.to_member("namespace")?.map(|v| v.try_into())?;
    Ok(MsfRemoveTrack { name, namespace })
}

/// cloneTracks 内のトラックオブジェクトをデコードする
///
/// draft-ietf-moq-msf-01 §5.1.6 (Delta update): cloned track は親の全属性を継承し、
/// 再定義された属性のみ上書きする。packaging や isLive は省略可。
/// この仕様は将来変更される可能性がある。
fn decode_clone_track(val: nojson::RawJsonValue<'_, '_>) -> Result<MsfCloneTrack, MessageError> {
    let name: String = val.to_member("name")?.required()?.try_into()?;

    // draft-ietf-moq-msf-01 §5.1.6 (Delta update): cloneTracks 内では parentName 必須
    let parent_name: String = val
        .to_member("parentName")?
        .required()?
        .try_into()
        .map_err(|e: nojson::JsonParseError| MessageError::InvalidCatalog(e.to_string()))?;

    // draft-ietf-moq-msf-01 §5.2.34 (Parent namespace): 省略時はカタログのネームスペースが
    // 親のネームスペースと仮定される。この仕様は将来変更される可能性がある。
    let parent_namespace: Option<String> =
        val.to_member("parentNamespace")?.map(|v| v.try_into())?;

    let packaging: Option<MsfPackaging> = match val.to_member("packaging")?.optional() {
        Some(v) => {
            let s: String = v.try_into()?;
            Some(MsfPackaging::from_packaging_str(&s)?)
        }
        None => None,
    };

    let f = decode_common_track_fields(val)?;
    let CommonTrackFields {
        namespace,
        event_type,
        role,
        target_latency,
        buffers,
        label,
        render_group,
        alt_group,
        init_ref,
        depends,
        template,
        temporal_id,
        spatial_id,
        codec,
        mime_type,
        framerate,
        timescale,
        bitrate,
        avg_bitrate,
        max_gop_duration,
        max_group_duration,
        width,
        height,
        samplerate,
        channel_config,
        display_width,
        display_height,
        lang,
        track_duration,
        connection_uri,
        token,
        encryption_scheme,
        cipher_suite,
        key_id,
        track_base_key,
        auth_info,
        accessibility,
        parent_name: _,
        parent_namespace: _,
    } = f;
    let is_live: Option<bool> = val.to_member("isLive")?.map(bool::try_from)?;

    // 暗号化 signaling の欠如検証は行わない。clone は親の属性を継承するため、
    // 断片単体では MUST 違反と断定できない (decode_track のみで検証する)。

    // packaging が明示されている場合も eventType の欠如は検証しない:
    // clone は親の属性を継承する (draft-ietf-moq-msf-01 §5.1.6 (Delta update):
    // "The cloned track inherits all attributes from the parent except the Track Name") ため、
    // packaging を eventtimeline と明示して eventType を省略した clone は、親が
    // eventtimeline (親は §5.2.5 の必須により eventType を持つ) ならば正当な継承表現であり、
    // decode 時点では親の属性を知らないため必須違反と断定できない。
    // 検証するのは「packaging 明示 + 非 eventtimeline なのに eventType が存在する」方向のみ
    // (packaging 省略 + eventType ありも親からの継承で正当化されるため受理する)。
    if let Some(ref pkg) = packaging {
        let is_event_timeline = matches!(pkg, MsfPackaging::EventTimeline);
        if !is_event_timeline && event_type.is_some() {
            return Err(MessageError::InvalidCatalog(
                "eventType MUST NOT be used when packaging is not 'eventtimeline'".to_string(),
            ));
        }
    }

    // 共存検査は isLive の値・有無によらず行い、trackDuration は isLive=true の場合のみ検証する
    // (本実装では presence 違反を優先して reject する選択とする。省略時は親から継承され得るが、
    // 断片単体では判定できないため fragment 内の両 key 存在で reject する)
    // draft-ietf-moq-msf-01 §5.2.8 (Target latency) / §5.2.9 (Buffers):
    // isLive=false なら targetLatency / buffers は無視されるため None に統一する。
    // None 化より前に key 存在で判定する (パース済み値では None 化で情報が落ちるため)。
    reject_coexisting_target_latency_buffers(val)?;
    let (target_latency, buffers) = if is_live == Some(false) {
        (None, None)
    } else {
        (target_latency, buffers)
    };
    if is_live == Some(true) && track_duration.is_some() {
        return Err(MessageError::InvalidCatalog(
            "trackDuration MUST NOT be included when isLive is true".to_string(),
        ));
    }

    Ok(MsfCloneTrack {
        name,
        parent_name,
        parent_namespace,
        packaging,
        namespace,
        event_type,
        role,
        is_live,
        target_latency,
        buffers,
        label,
        render_group,
        alt_group,
        init_ref,
        depends,
        template,
        temporal_id,
        spatial_id,
        codec,
        mime_type,
        framerate,
        timescale,
        bitrate,
        avg_bitrate,
        max_gop_duration,
        max_group_duration,
        width,
        height,
        samplerate,
        channel_config,
        display_width,
        display_height,
        lang,
        track_duration,
        connection_uri,
        token,
        encryption_scheme,
        cipher_suite,
        key_id,
        track_base_key,
        auth_info,
        accessibility,
    })
}

fn decode_media_entry(
    record_v: nojson::RawJsonValue<'_, '_>,
) -> Result<MsfMediaTimelineEntry, MessageError> {
    let mut iter = record_v.to_array()?;
    let pts_v = iter.next().ok_or_else(|| {
        MessageError::InvalidCatalog("media timeline record: missing pts_ms".to_string())
    })?;
    let loc_v = iter.next().ok_or_else(|| {
        MessageError::InvalidCatalog("media timeline record: missing location".to_string())
    })?;
    let wall_v = iter.next().ok_or_else(|| {
        MessageError::InvalidCatalog(
            "media timeline record must have exactly 3 elements".to_string(),
        )
    })?;
    if iter.next().is_some() {
        return Err(MessageError::InvalidCatalog(
            "media timeline record has more than 3 elements".to_string(),
        ));
    }

    let pts_ms = parse_u64(pts_v)?;

    let mut loc_iter = loc_v.to_array()?;
    let g_v = loc_iter.next().ok_or_else(|| {
        MessageError::InvalidCatalog("media timeline location: missing group_id".to_string())
    })?;
    let o_v = loc_iter.next().ok_or_else(|| {
        MessageError::InvalidCatalog(
            "media timeline location must have exactly 2 elements".to_string(),
        )
    })?;
    if loc_iter.next().is_some() {
        return Err(MessageError::InvalidCatalog(
            "media timeline location has more than 2 elements".to_string(),
        ));
    }

    let group_id = parse_u64(g_v)?;
    let object_id = parse_u64(o_v)?;
    let wallclock_ms = parse_u64(wall_v)?;

    Ok(MsfMediaTimelineEntry {
        pts_ms,
        group_id,
        object_id,
        wallclock_ms,
    })
}

fn decode_event_entry(
    val: nojson::RawJsonValue<'_, '_>,
) -> Result<MsfEventTimelineEntry, MessageError> {
    // draft-ietf-moq-msf-01 §8.1 (Event Timeline data format): index は t/l/m のうち 1 つだけ
    let has_l = val.to_member("l")?.optional().is_some();
    let has_t = val.to_member("t")?.optional().is_some();
    let has_m = val.to_member("m")?.optional().is_some();
    let index_count = has_l as u8 + has_t as u8 + has_m as u8;
    if index_count == 0 {
        return Err(MessageError::InvalidCatalog(
            "event record missing index field ('t', 'l', or 'm')".to_string(),
        ));
    }
    if index_count > 1 {
        return Err(MessageError::InvalidCatalog(
            "event record MUST have only one of 't', 'l', or 'm'".to_string(),
        ));
    }

    let index = if let Some(l_v) = val.to_member("l")?.optional() {
        let mut iter = l_v.to_array()?;
        let g_v = iter.next().ok_or_else(|| {
            MessageError::InvalidCatalog("event 'l': missing group_id".to_string())
        })?;
        let o_v = iter.next().ok_or_else(|| {
            MessageError::InvalidCatalog("event 'l' must have exactly 2 elements".to_string())
        })?;
        if iter.next().is_some() {
            return Err(MessageError::InvalidCatalog(
                "event 'l' has more than 2 elements".to_string(),
            ));
        }
        MsfEventIndex::Location(parse_u64(g_v)?, parse_u64(o_v)?)
    } else if let Some(t_v) = val.to_member("t")?.optional() {
        MsfEventIndex::WallclockMs(parse_u64(t_v)?)
    } else {
        // has_m は true (上で index_count == 1 を保証済み)
        let m_v = val.to_member("m")?.required()?;
        MsfEventIndex::MediaPtsMs(parse_u64(m_v)?)
    };

    let data_v = val.to_member("data")?.required()?;
    // draft-ietf-moq-msf-01 §8.1 (Event Timeline data format): data は Object でなければならない
    // この仕様は将来変更される可能性がある。
    // to_object() で型チェックだけ行い、生 JSON として保持する
    let _ = data_v.to_object().map_err(|_| {
        MessageError::InvalidCatalog("event data MUST be a JSON object".to_string())
    })?;
    let data_raw = data_v.as_raw_str().as_bytes().to_vec();

    Ok(MsfEventTimelineEntry { index, data_raw })
}

/// 認可情報の生 JSON 値を検証する
///
/// draft-ietf-moq-msf-01 §5.2.42 (Authorization Info): 値は scheme 固有の
/// 単独の JSON 値でなければならない。encode 側の検証欠落による panic
/// (`RawJsonBytes` の fmt エラー時の `to_string` の仕様) と不正な JSON の
/// 静的な生成を防ぐため、`value_raw` が「UTF-8 かつ単独の JSON 値として
/// パース可能」であることを検証する。エラー種別・文言は decode 側と対称にする。
/// この仕様は将来変更される可能性がある。
fn validate_auth_info_value_raw(value_raw: &[u8]) -> Result<(), MessageError> {
    let text = core::str::from_utf8(value_raw).map_err(|_| {
        MessageError::InvalidCatalog("authInfo value is not valid UTF-8".to_string())
    })?;
    let raw = nojson::RawJson::parse(text)?;
    let _ = raw.value();
    Ok(())
}

/// イベントデータの生 JSON バイト列を検証する
///
/// draft-ietf-moq-msf-01 §8.1 (Event Timeline data format): "An event timeline track is a JSON [JSON] document."
/// この仕様は将来変更される可能性がある。
/// encode 側の検証欠落による panic (fmt エラー時の to_string の仕様) と
/// 不正な JSON の静的な生成を防ぐため、`data_raw` が「UTF-8 かつ単独の JSON object
/// としてパース可能」であることを検証する。エラー種別・文言は decode 側と対称にする。
fn validate_event_data_raw(data_raw: &[u8]) -> Result<(), MessageError> {
    let text = core::str::from_utf8(data_raw).map_err(|_| {
        MessageError::InvalidCatalog("event timeline is not valid UTF-8".to_string())
    })?;
    let raw = nojson::RawJson::parse(text)?;
    let val = raw.value();
    let _ = val.to_object().map_err(|_| {
        MessageError::InvalidCatalog("event data MUST be a JSON object".to_string())
    })?;
    Ok(())
}

// ─── Timeline の gzip 圧縮・自動復号 ────────────────────────────────────────
//
// §7.1 (Media Timeline track payload) および §8.1 (Event Timeline data format) は
// payload の compression を許容する。本実装は gzip (RFC 1952) magic byte 検出による
// 自動復号と任意の gzip 圧縮を提供する。-01 で導入された MSF_COMPRESSION property
// signaling 機構 (§12.1 (Compression Signaling)) には未対応 (将来の draft 改訂時に実装予定)。

/// Media / Event Timeline のエンコードオプション
///
/// `gzip == true` の場合、JSON バイト列を gzip で圧縮してから返す
/// (draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload) / §8.1 (Event Timeline data format))。
#[derive(Debug, Clone, Copy, Default)]
pub struct TimelineEncodingOptions {
    /// true の場合は JSON バイト列を gzip 圧縮する
    pub gzip: bool,
}

/// gzip magic `0x1F 0x8B` を検出して圧縮済みか判定する
fn is_gzip_compressed(data: &[u8]) -> bool {
    matches!(noflate::Format::detect(data), Some(noflate::Format::Gzip))
}

/// gzip 展開後の累計出力サイズの上限 (16 MiB)
///
/// この上限は実装が定める防御的上限であり、仕様で規定された値ではない。
/// draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload) / §8.1 (Event Timeline data format) は MSF_COMPRESSION property (Section 12.1) による圧縮のみ規定し、
/// 展開後サイズや圧縮比の上限を定めていない。draft-ietf-moq-msf-01 §13 (Security Considerations)
/// も上限値を規定していないため、本実装は OOM 防止の防御として独自上限を置く。
/// GZIP 自体 (RFC 1952) も展開後サイズを規定しない。
///
/// 16 MiB の根拠:
/// 現実的な Media / Event Timeline は記録あたり数十バイトのメタデータ JSON であり、
/// 数十万記録でも単桁 MB に収まる。16 MiB は正当なコンテンツを誤って弾かない余裕を持ちつつ、
/// OOM (GB 級) には遠く及ばない値。実測で現実的サイズがこれを超える場合は調整してよい。
const MAX_DECOMPRESSED_TIMELINE_BYTES: usize = 16 * 1024 * 1024;

/// 1 回の `feed` に渡す圧縮入力チャンクサイズ
///
/// DEFLATE の理論上、1 feed あたりの展開出力は最大で
/// `FEED_CHUNK × 約 1032` バイトに有界化される
/// (約 1032:1 = 最大マッチ長 258 バイト ÷ 最小符号長 2 ビット × 8。DEFLATE の理論上界の概算)。
/// 各 feed 後に累計出力を上限と照合して即座に打ち切ることで、
/// 全体の展開後サイズを `MAX_DECOMPRESSED_TIMELINE_BYTES` 内に抑える。
///
/// ピークメモリの目安:
/// `MAX_DECOMPRESSED_TIMELINE_BYTES + 2 × (FEED_CHUNK × 約 1032)`。
/// FEED_CHUNK = 4096 で 1 feed あたりの transient は約 4 MiB。
const FEED_CHUNK: usize = 4096;

/// 必要に応じて gzip 展開し、生 JSON バイト列を返す
///
/// ストリーミング展開により、展開後サイズを
/// `MAX_DECOMPRESSED_TIMELINE_BYTES` 以内に制限する。
/// 上限を超えた場合は `MessageError::GzipDecode` を返す。
/// 非 gzip 入力はそのままコピーして返す。
fn maybe_decompress_gzip(data: &[u8]) -> Result<Vec<u8>, MessageError> {
    if !is_gzip_compressed(data) {
        return Ok(data.to_vec());
    }

    let mut decoder = noflate::gzip::Decoder::new();
    let mut out = Vec::new();

    for chunk in data.chunks(FEED_CHUNK) {
        decoder
            .feed(chunk)
            .map_err(|e| MessageError::GzipDecode(format!("{e}")))?;
        let produced = decoder.output();
        if out.len() + produced.len() > MAX_DECOMPRESSED_TIMELINE_BYTES {
            return Err(MessageError::GzipDecode(
                "decompressed timeline exceeds limit".into(),
            ));
        }
        out.extend_from_slice(produced);
        let n = produced.len();
        decoder.advance(n);
    }

    // trailer まで到達せず入力が尽きた (truncated) ケースを捕捉する
    if !decoder.is_finished() {
        return Err(MessageError::GzipDecode(
            "gzip stream ended before trailer".into(),
        ));
    }
    Ok(out)
}

/// JSON バイト列を必要に応じて gzip 圧縮する
fn maybe_compress_gzip(
    json: Vec<u8>,
    options: TimelineEncodingOptions,
) -> Result<Vec<u8>, MessageError> {
    if options.gzip {
        noflate::gzip::compress(&json).map_err(|e| MessageError::GzipEncode(format!("{e}")))
    } else {
        Ok(json)
    }
}

/// Media Timeline を JSON にシリアライズし、オプションで gzip 圧縮する
///
/// draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload) に準拠。
pub fn encode_media_timeline(
    timeline: &MsfMediaTimeline,
    options: TimelineEncodingOptions,
) -> Result<Vec<u8>, MessageError> {
    maybe_compress_gzip(timeline.encode(), options)
}

/// Media Timeline バイト列をデコードする。先頭に gzip magic があれば自動展開する
///
/// draft-ietf-moq-msf-01 §7.1 (Media Timeline track payload) に準拠。
pub fn decode_media_timeline(data: &[u8]) -> Result<MsfMediaTimeline, MessageError> {
    let json = maybe_decompress_gzip(data)?;
    MsfMediaTimeline::decode(&json)
}

/// Event Timeline を JSON にシリアライズし、オプションで gzip 圧縮する
///
/// draft-ietf-moq-msf-01 §8.1 (Event Timeline data format) に準拠。
pub fn encode_event_timeline(
    timeline: &MsfEventTimeline,
    options: TimelineEncodingOptions,
) -> Result<Vec<u8>, MessageError> {
    maybe_compress_gzip(timeline.encode()?, options)
}

/// Event Timeline バイト列をデコードする。先頭に gzip magic があれば自動展開する
///
/// draft-ietf-moq-msf-01 §8.1 (Event Timeline data format) に準拠。
pub fn decode_event_timeline(data: &[u8]) -> Result<MsfEventTimeline, MessageError> {
    let json = maybe_decompress_gzip(data)?;
    MsfEventTimeline::decode(&json)
}

// ─── 変数置換 (draft-ietf-moq-msf-01 §5.4 (Variable Substitution)) ─────────────
//
// カタログ decode の前処理として使う。decode 層自体は `%` を検証しない。

/// 変数名として有効な文字か (英数字・`-`・`_`)
///
/// draft-ietf-moq-msf-01 §5.4.1 (Variable Syntax) に準拠。
fn is_variable_name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// 変数値として有効な文字か (英数字・`-`・`_`・`@`)
///
/// draft-ietf-moq-msf-01 §5.4.1 (Variable Syntax) に準拠。
fn is_variable_value_char(b: u8) -> bool {
    is_variable_name_char(b) || b == b'@'
}

/// fragment 文字列を key-value 対にパースする
///
/// draft-ietf-moq-msf-01 §5.4.2 (Variable Resolution) に準拠し、`&` 区切り・
/// `=` 分離で解釈する。`=` を含まない要素は無視する。空の変数名・変数名文字種外・
/// 値文字種外は reject する。重複キーは両方保持する。空値は空文字列として受理する。
/// query (`?` 以降) は呼出層で除外し、`#` を含まない `#` 以降の fragment 全体
/// (`msf:` 含む) を渡すこと。percent-decode は行わず、値をそのまま使う。
/// 単独利用時は変数 fragment の検査用。`resolve_catalog_variables` と異なり
/// 予約パラメータ混じりは reject するため、実 URI 由来の fragment 解決には
/// `resolve_catalog_variables` を使うこと。
///
/// # Errors
///
/// - 空の変数名・変数名文字種外・値文字種外・`#` 含有: `InvalidCatalog`
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
pub fn parse_fragment_pairs(fragment: &str) -> Result<Vec<(String, String)>, MessageError> {
    if fragment.contains('#') {
        return Err(MessageError::InvalidCatalog(
            "invalid fragment: '#' is not allowed (pass only the part after '#')".to_string(),
        ));
    }
    let mut pairs = Vec::new();
    if fragment.is_empty() {
        return Ok(pairs);
    }
    for element in fragment.split('&') {
        let Some((key, value)) = element.split_once('=') else {
            continue;
        };
        if key.is_empty() || !key.bytes().all(is_variable_name_char) {
            return Err(MessageError::InvalidCatalog(format!(
                "invalid variable name '{key}'"
            )));
        }
        if !value.bytes().all(is_variable_value_char) {
            return Err(MessageError::InvalidCatalog(format!(
                "invalid variable value for '{key}'"
            )));
        }
        pairs.push((key.to_string(), value.to_string()));
    }
    Ok(pairs)
}

/// fragment を検証なしで key-value に分割する (resolve 内部用)
///
/// §11.1 の予約パラメータ等、変数以外の対を含み得るため文字種検証は行わない。
/// `=` を含まない要素は無視する。変数名として不正な key は正規の参照と
/// 一致しないため置換対象にならない。値文字種は置換時に検証する。
fn split_fragment_pairs_unchecked(fragment: &str) -> Vec<(String, String)> {
    fragment
        .split('&')
        .filter_map(|element| element.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// catalog JSON の変数参照 (`%name%`) を解決する
///
/// draft-ietf-moq-msf-01 §5.4 (Variable Substitution) に準拠する。
/// 置換は decode 前の生 JSON に対して行い、decode 層は変更しない。
/// 未定義変数の参照は保持する (§5.4.2 に未定義時の規定がないため。decode 層は
/// `%` を検証しないため、そのまま通過する)。閉じない `%`・空名・
/// 変数名不正文字・リテラル `%` は reject する。呼出層が URI から `#` 以降全体
/// (`msf:` 含む) を渡すこと。先頭要素 (track-identifier 等の `=` なし要素) は無視する。
/// fragment 内の予約パラメータ等 (変数以外) は無視し、参照された値のみ文字種検証する。
/// 重複キーは先勝ちで解決する。
///
/// 制限: 生 JSON を走査するため、キー部や JSON エスケープ (`\u0025` 等) との
/// 区別はしない。有効なカタログのキーは固定であり `%` を含まないため実害はないが、
/// 厳密には値限定の仕様より広く置換する。`%` は ASCII のため subslice が
/// 文字境界を割ることはない。percent-decode は行わず、値をそのまま使う。
///
/// # Errors
///
/// - JSON 非 UTF-8 ・`#` 含有・不正な変数参照・不正な置換値: `InvalidCatalog`
///   (変数参照エラーは位置付き。不正参照の開始バイト位置を含む)
///
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
pub fn resolve_catalog_variables(json: &[u8], fragment: &str) -> Result<Vec<u8>, MessageError> {
    let text = core::str::from_utf8(json)
        .map_err(|_| MessageError::InvalidCatalog("catalog is not valid UTF-8".to_string()))?;
    if fragment.contains('#') {
        return Err(MessageError::InvalidCatalog(
            "invalid fragment: '#' is not allowed (pass only the part after '#')".to_string(),
        ));
    }
    let pairs = split_fragment_pairs_unchecked(fragment);
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut pos = 0;
    while pos < bytes.len() {
        if bytes[pos] != b'%' {
            out.push(bytes[pos]);
            pos += 1;
            continue;
        }
        // `%` 以降の次 `%` までを変数名として切り出す
        let rest = &bytes[pos + 1..];
        let Some(end) = rest.iter().position(|&b| b == b'%') else {
            return Err(MessageError::InvalidCatalog(format!(
                "unclosed variable reference at byte {pos}"
            )));
        };
        let name = &rest[..end];
        if name.is_empty() || !name.iter().all(|&b| is_variable_name_char(b)) {
            return Err(MessageError::InvalidCatalog(format!(
                "invalid variable reference at byte {pos}"
            )));
        }
        let name_str = core::str::from_utf8(name).map_err(|_| {
            // `%` 区切りのため到達不能のはずだが防御的に残す
            MessageError::InvalidCatalog(format!("invalid variable reference at byte {pos}"))
        })?;
        match pairs.iter().find(|(key, _)| key == name_str) {
            Some((_, value)) => {
                // 置換時に値文字種を検証する (予約パラメータ等は参照されなければ不問)
                if !value.bytes().all(is_variable_value_char) {
                    return Err(MessageError::InvalidCatalog(format!(
                        "invalid variable value for '{name_str}'"
                    )));
                }
                out.extend_from_slice(value.as_bytes());
            }
            // 未定義変数は保持する (§5.4.2 に未定義時の規定がないため)
            None => {
                out.push(b'%');
                out.extend_from_slice(name);
                out.push(b'%');
            }
        }
        pos += 1 + end + 1;
    }
    Ok(out)
}

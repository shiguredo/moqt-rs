//! MSF_COMPRESSION property のセッション層解釈
//!
//! draft-ietf-moq-msf-02 §12.1 (Compression Signaling) は Track Property (§12.1.1) と
//! Object Property (§12.1.2) の 2 つの signaling 機構を定義し、同じ track で両方を使っては
//! ならない (MUST NOT) と定める。このモジュールは、受信した Track / Object Properties から
//! payload の圧縮方式を解決し、両機構の併用を [`MessageError::MalformedTrack`] として
//! 拒否する。publisher 側の送信検証は [`resolve`] の結果を `ProtocolViolation` に写して使う。
//!
//! 0 / 1 以外の値は §12.1 の MUST NOT attempt to process に従い、
//! [`MessageError::UnsupportedCompression`] として扱う (セッションは閉じない)。

use crate::error::MessageError;
use crate::msf::MsfCompression;
use crate::object_properties::ObjectProperties;
use crate::track_properties::{PROP_IMMUTABLE_PROPERTIES, PROP_MSF_COMPRESSION, TrackProperties};

/// Track Properties から MSF_COMPRESSION を解決する
///
/// property が無ければ `None`、値 0 は `Some(MsfCompression::None)`、値 1 は
/// `Some(MsfCompression::Gzip)` を返す。値 2 以上は `UnsupportedCompression` を返す。
pub(crate) fn track_compression(
    properties: &TrackProperties,
) -> Result<Option<MsfCompression>, MessageError> {
    properties
        .msf_compression()
        .map(MsfCompression::try_from)
        .transpose()
}

/// Object Properties から MSF_COMPRESSION を解決する
///
/// 戻り値の意味は [`track_compression`] と同じである。
pub(crate) fn object_compression(
    properties: &ObjectProperties,
) -> Result<Option<MsfCompression>, MessageError> {
    properties
        .msf_compression()
        .map(MsfCompression::try_from)
        .transpose()
}

/// Track / Object の signaling を統合して payload の圧縮方式を決める
///
/// `Some(MsfCompression::None)` は「property が存在し、値が 0」を表す。track と object の
/// 両方に property がある場合は §12.1 の MUST NOT に反するため `MalformedTrack` を返す。
pub(crate) fn resolve(
    track: Option<MsfCompression>,
    object: Option<MsfCompression>,
) -> Result<MsfCompression, MessageError> {
    match (track, object) {
        (Some(_), Some(_)) => Err(MessageError::MalformedTrack(
            "track and object MSF_COMPRESSION are both set",
        )),
        (Some(compression), None) | (None, Some(compression)) => Ok(compression),
        (None, None) => Ok(MsfCompression::None),
    }
}

/// `INCLUDE_PROPERTIES=0` の応答でも保持する property だけを残す
///
/// draft-ietf-moq-msf-02 §12.1.1 は MSF_COMPRESSION Track Property を PUBLISH /
/// SUBSCRIBE_OK / FETCH_OK に含めることを MUST とする。draft-ietf-moq-transport-23
/// §9.19.22 (INCLUDE_PROPERTIES Parameter) の空化は SHOULD であるため、MSF の MUST が
/// 優先する。0x78 が IMMUTABLE_PROPERTIES (0x0B) の内側にある場合に備えて、
/// IMMUTABLE_PROPERTIES 自体も残す。他の Track Property は落とす。
pub(crate) fn retain_msf_compression(properties: &TrackProperties) -> TrackProperties {
    let mut retained = TrackProperties::new();
    for property in properties.as_slice() {
        if matches!(
            property.prop_type,
            PROP_MSF_COMPRESSION | PROP_IMMUTABLE_PROPERTIES
        ) {
            retained.push(property.clone());
        }
    }
    retained
}

impl crate::session::core::Session {
    /// 自側が送った Track Properties から MSF_COMPRESSION を記録する
    ///
    /// property が無ければ既存の記録を削除する。値 0 (非圧縮) も「Track Property を
    /// 設定した」として記録する。
    pub(super) fn record_local_msf_compression(
        &mut self,
        request_id: u64,
        properties: &TrackProperties,
    ) {
        match properties.msf_compression() {
            Some(value) => {
                self.local_msf_compression.insert(request_id, value);
            }
            None => {
                self.local_msf_compression.remove(&request_id);
            }
        }
    }

    /// 自側が送った MSF_COMPRESSION Track Property があるかを返す
    pub(super) fn has_local_msf_compression(&self, request_id: u64) -> bool {
        self.local_msf_compression.contains_key(&request_id)
    }
}

/// Object Properties の生バイト列に MSF_COMPRESSION が含まれるかを返す
///
/// decode に失敗した場合は `false` を返す (不正な properties の拒否は wire encode 層の
/// 責務であり、ここでは併用検出だけを行う)。
pub(crate) fn object_properties_have_compression(properties_bytes: Option<&[u8]>) -> bool {
    properties_bytes
        .and_then(|bytes| ObjectProperties::decode(bytes).ok())
        .is_some_and(|(properties, _)| properties.msf_compression().is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object_properties::{ObjectProperty, ObjectPropertyValue};
    use crate::track_properties::{TrackProperty, TrackPropertyValue};
    use alloc::vec;

    /// テスト用の Track Property を 1 つ作る
    fn track_property(prop_type: u64, value: u64) -> TrackProperty {
        TrackProperty {
            prop_type,
            value: TrackPropertyValue::VarInt(value),
        }
    }

    /// テスト用の Object Property を 1 つ作る
    fn object_property(prop_type: u64, value: u64) -> ObjectProperty {
        ObjectProperty {
            prop_type,
            value: ObjectPropertyValue::VarInt(value),
        }
    }

    /// Track Property が無ければ None になること
    #[test]
    fn absent_track_property_is_none() {
        assert_eq!(
            track_compression(&TrackProperties::new()).expect("空の Track Properties は有効"),
            None
        );
    }

    /// Track Property の値 0 / 1 を解決できること
    #[test]
    fn track_property_values_are_resolved() {
        let mut none = TrackProperties::new();
        none.push(track_property(PROP_MSF_COMPRESSION, 0));
        assert_eq!(
            track_compression(&none).expect("値 0 はサポートされる"),
            Some(MsfCompression::None)
        );
        let mut gzip = TrackProperties::new();
        gzip.push(track_property(PROP_MSF_COMPRESSION, 1));
        assert_eq!(
            track_compression(&gzip).expect("値 1 はサポートされる"),
            Some(MsfCompression::Gzip)
        );
    }

    /// Track Property の未サポート値は UnsupportedCompression になること
    #[test]
    fn unsupported_track_property_is_rejected() {
        let mut properties = TrackProperties::new();
        properties.push(track_property(PROP_MSF_COMPRESSION, 2));
        assert_eq!(
            track_compression(&properties),
            Err(MessageError::UnsupportedCompression(2))
        );
    }

    /// Object Property が無ければ None になること
    #[test]
    fn absent_object_property_is_none() {
        assert_eq!(
            object_compression(&ObjectProperties::new()).expect("空の Object Properties は有効"),
            None
        );
    }

    /// Object Property の値 1 を解決できること
    #[test]
    fn object_property_gzip_is_resolved() {
        let mut properties = ObjectProperties::new();
        properties.push(object_property(PROP_MSF_COMPRESSION, 1));
        assert_eq!(
            object_compression(&properties).expect("値 1 はサポートされる"),
            Some(MsfCompression::Gzip)
        );
    }

    /// Object Property の未サポート値は UnsupportedCompression になること
    #[test]
    fn unsupported_object_property_is_rejected() {
        let mut properties = ObjectProperties::new();
        properties.push(object_property(PROP_MSF_COMPRESSION, 2));
        assert_eq!(
            object_compression(&properties),
            Err(MessageError::UnsupportedCompression(2))
        );
    }

    /// Track と Object の両方の signaling があると MalformedTrack になること
    #[test]
    fn both_signaling_mechanisms_are_rejected() {
        assert_eq!(
            resolve(Some(MsfCompression::Gzip), Some(MsfCompression::Gzip)),
            Err(MessageError::MalformedTrack(
                "track and object MSF_COMPRESSION are both set"
            ))
        );
        // track の値が 0 (uncompressed) でも、2 つの機構の併用は MUST NOT に反する
        assert!(matches!(
            resolve(Some(MsfCompression::None), Some(MsfCompression::None)),
            Err(MessageError::MalformedTrack(_))
        ));
    }

    /// 片方だけの signaling と、どちらも無い場合を解決できること
    #[test]
    fn single_or_absent_signaling_is_resolved() {
        assert_eq!(
            resolve(Some(MsfCompression::Gzip), None).expect("track の signaling は有効"),
            MsfCompression::Gzip
        );
        assert_eq!(
            resolve(None, Some(MsfCompression::Gzip)).expect("object の signaling は有効"),
            MsfCompression::Gzip
        );
        assert_eq!(
            resolve(None, None).expect("property 無しは非圧縮"),
            MsfCompression::None
        );
    }

    /// INCLUDE_PROPERTIES=0 でも MSF_COMPRESSION と IMMUTABLE_PROPERTIES を残すこと
    #[test]
    fn retain_keeps_msf_compression() {
        let mut properties = TrackProperties::new();
        properties.push(track_property(0x30, 1)); // DYNAMIC_GROUPS
        properties.push(track_property(PROP_MSF_COMPRESSION, 1));
        properties.push(TrackProperty {
            prop_type: PROP_IMMUTABLE_PROPERTIES,
            // 内側に MSF_COMPRESSION がある想定のバイト列 (中身は検証しない)
            value: TrackPropertyValue::Bytes(vec![0x78, 0x01]),
        });
        let retained = retain_msf_compression(&properties);
        assert_eq!(
            retained.msf_compression(),
            Some(1),
            "MSF_COMPRESSION は INCLUDE_PROPERTIES=0 でも残す"
        );
        assert_eq!(
            retained.as_slice().len(),
            2,
            "MSF_COMPRESSION と IMMUTABLE_PROPERTIES だけを残す"
        );
        assert_eq!(retained.dynamic_groups(), None);
    }
}

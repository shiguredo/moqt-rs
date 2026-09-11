//! MSF カタログの生成と送信
//!
//! video / audio track の情報から MSF カタログを組み立て、catalog track に送信する。
//! draft-ietf-moq-msf-01 §5 (Catalog) に準拠する。

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::{
    msf::MSF_VERSION, msf::MsfCatalog, msf::MsfCatalogDocument, msf::MsfPackaging, msf::MsfTrack,
};

use crate::error::{Error, Result};
use crate::stream_writer::SubgroupWriter;
use moqt_example_transport::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use moqt_example_transport::transport;

/// 映像トラックの catalog 情報
pub struct VideoTrackParams<'a> {
    pub track_name: &'a str,
    pub namespace: &'a str,
    /// RFC 6381 形式の codec 文字列 (例: `av01.0.08M.08`)
    pub codec: &'a str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// kbps
    pub bitrate: u32,
}

/// 音声トラックの catalog 情報
pub struct AudioTrackParams<'a> {
    pub track_name: &'a str,
    pub namespace: &'a str,
    /// RFC 6381 形式の codec 文字列 (例: `opus`)
    pub codec: &'a str,
    /// サンプリングレート Hz
    pub samplerate: u32,
    /// MSF channelConfig (mono = "1", stereo = "2")
    /// draft-ietf-moq-msf-01 §5.2.29 (Channel configuration)
    pub channel_config: &'a str,
    /// kbps
    pub bitrate: u32,
}

/// カタログ送信用パラメータ
pub struct CatalogParams<'a> {
    pub handle: &'a transport::StreamHandle,
    pub data_plane: &'a DataPlaneHandle,
    pub catalog_request_id: u64,
    pub catalog_alias: u64,
    pub video: Option<VideoTrackParams<'a>>,
    pub audio: Option<AudioTrackParams<'a>>,
}

/// MSF カタログを構築して送信する
///
/// カタログトラックとして Group 0 / Object 0 に Full カタログを送信する。
/// draft-ietf-moq-msf-01 §5 (Catalog)
pub async fn send_catalog(params: CatalogParams<'_>) -> Result<()> {
    let mut tracks = Vec::new();

    if let Some(v) = &params.video {
        let mut track = MsfTrack::new(v.track_name.to_string(), MsfPackaging::Loc, true);
        track.namespace = Some(v.namespace.to_string());
        track.codec = Some(v.codec.to_string());
        track.width = Some(v.width as u64);
        track.height = Some(v.height as u64);
        track.framerate = Some(v.fps as f64);
        track.bitrate = Some(v.bitrate as u64 * 1000);
        tracks.push(track);
    }

    if let Some(a) = &params.audio {
        let mut track = MsfTrack::new(a.track_name.to_string(), MsfPackaging::Loc, true);
        track.namespace = Some(a.namespace.to_string());
        track.codec = Some(a.codec.to_string());
        track.samplerate = Some(a.samplerate as u64);
        track.channel_config = Some(a.channel_config.to_string());
        track.bitrate = Some(a.bitrate as u64 * 1000);
        tracks.push(track);
    }

    if tracks.is_empty() {
        return Err(Error::Other(
            "catalog must contain at least one track".to_string(),
        ));
    }

    let catalog = MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks,
        publish_tracks: Vec::new(),
        init_data_list: Vec::new(),
    });

    let catalog_json = catalog
        .encode()
        .map_err(|e| Error::Other(format!("catalog encode: {e}")))?;
    tracing::info!(
        "Sending MSF catalog ({} bytes): {}",
        catalog_json.len(),
        String::from_utf8_lossy(&catalog_json)
    );

    // カタログを Subgroup Stream として送信する
    let mut writer = SubgroupWriter::new(
        params.handle,
        params.data_plane,
        params.catalog_request_id,
        params.catalog_alias,
        0,
        128,
        false,
    )
    .await?;
    let empty_props = LocProperties::new();
    let outcome = writer.write_object(&catalog_json, &empty_props).await?;
    if outcome == ObjectFilterOutcome::Skip {
        // 全 Skip の writer を reset で終端してから、カタログが届かないことをエラーとして報告する
        // (カタログがフィルタ不通過で届かないと subscriber は起動できないため、静かに飲み込まない)
        writer.finish()?;
        return Err(Error::Other(
            "catalog object skipped by subscription filter".to_string(),
        ));
    }
    writer.finish()?;

    Ok(())
}

//! MSF カタログの生成と送信
//!
//! video / audio track の情報から MSF カタログを組み立て、catalog track に送信する。
//! draft-ietf-moq-msf-01 §5 (Catalog) に準拠する。

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::{
    msf::MSF_VERSION, msf::MsfCatalog, msf::MsfCatalogDocument, msf::MsfPackaging, msf::MsfTrack,
};

use crate::error::{Error, Result};
use crate::stream_writer::SubgroupWriter;
use tokio_moq::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use tokio_moq::transport;

/// 映像トラックの role (draft-ietf-moq-msf-01 §5.2.6 (Track role) Table 4)
const MSF_ROLE_VIDEO: &str = "video";

/// 音声トラックの role (draft-ietf-moq-msf-01 §5.2.6 (Track role) Table 4)
const MSF_ROLE_AUDIO: &str = "audio";

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

/// publisher が音声・映像の両トラックへ共通で載せる同期用のメタデータ
///
/// 音声と映像を同じ時間軸で再生するため、両トラックへ同じ値を載せる。
/// draft-ietf-moq-msf-01 §5.2.8 (Target latency) は同じ render group のトラックに
/// 同一の targetLatency を MUST で求める。
#[derive(Debug, Clone, Copy)]
pub struct CatalogSyncParams {
    /// レンダーグループ
    ///
    /// 同じ値を持つトラックは同時に再生する (draft-ietf-moq-msf-01 §5.2.11)。
    pub render_group: u64,
    /// ターゲットレイテンシ (ms)
    ///
    /// 符号化した時刻から表示するまでのずれ (draft-ietf-moq-msf-01 §5.2.8)。
    pub target_latency_ms: u64,
}

/// カタログ送信用パラメータ
pub struct CatalogParams<'a> {
    pub handle: &'a transport::StreamHandle,
    pub data_plane: &'a DataPlaneHandle,
    pub catalog_request_id: u64,
    pub catalog_alias: u64,
    /// カタログ track の購読の Start Location (`MoqtClient::subscription_filter_start`)
    ///
    /// カタログは 1 Subgroup = 1 Object なので終端方法の判定結果は変わらない (省略があれば
    /// `SUBGROUP_HEADER` 未送信で RESET、配送すれば FIN)。複数 Object を持つ Subgroup と同じ
    /// 形で呼び出し側から値を渡しておく。
    pub start_location: Option<Location>,
    /// 音声・映像で共通の同期用メタデータ
    pub sync: CatalogSyncParams,
    pub video: Option<VideoTrackParams<'a>>,
    pub audio: Option<AudioTrackParams<'a>>,
}

/// video / audio のパラメータから MSF カタログを構築する
///
/// video / audio のいずれも指定されない場合は、トラックを 1 つも持たないカタログになるため
/// 構築せずにエラーを返す。
/// draft-ietf-moq-msf-01 §5 (Catalog)
fn build_catalog(
    video: Option<&VideoTrackParams<'_>>,
    audio: Option<&AudioTrackParams<'_>>,
    sync: CatalogSyncParams,
) -> Result<MsfCatalogDocument> {
    let mut tracks = Vec::new();

    if let Some(v) = video {
        let mut track = MsfTrack::new(v.track_name.to_string(), MsfPackaging::Loc, true);
        track.namespace = Some(v.namespace.to_string());
        // draft-ietf-moq-msf-01 §5.2.6 (Track role) Table 4 の予約 role を載せる。
        // 受信側は codec だけでなく role からも content の種別を判定できる
        track.role = Some(MSF_ROLE_VIDEO.to_string());
        track.codec = Some(v.codec.to_string());
        track.width = Some(v.width as u64);
        track.height = Some(v.height as u64);
        track.framerate = Some(v.fps as f64);
        track.bitrate = Some(v.bitrate as u64 * 1000);
        track.target_latency = Some(sync.target_latency_ms);
        track.render_group = Some(sync.render_group);
        tracks.push(track);
    }

    if let Some(a) = audio {
        let mut track = MsfTrack::new(a.track_name.to_string(), MsfPackaging::Loc, true);
        track.namespace = Some(a.namespace.to_string());
        // draft-ietf-moq-msf-01 §5.2.6 (Track role) Table 4 の予約 role を載せる。
        // 受信側は codec だけでなく role からも content の種別を判定できる
        track.role = Some(MSF_ROLE_AUDIO.to_string());
        track.codec = Some(a.codec.to_string());
        track.samplerate = Some(a.samplerate as u64);
        track.channel_config = Some(a.channel_config.to_string());
        track.bitrate = Some(a.bitrate as u64 * 1000);
        track.target_latency = Some(sync.target_latency_ms);
        track.render_group = Some(sync.render_group);
        tracks.push(track);
    }

    if tracks.is_empty() {
        return Err(Error::Other(
            "catalog must contain at least one track".to_string(),
        ));
    }

    Ok(MsfCatalogDocument::Full(MsfCatalog {
        version: MSF_VERSION.to_string(),
        generated_at: None,
        is_complete: false,
        tracks,
        publish_tracks: Vec::new(),
        removed_tracks: Default::default(),
        init_data_list: Vec::new(),
    }))
}

/// MSF カタログを構築して送信する
///
/// カタログトラックとして Group 0 / Object 0 に Full カタログを送信する。
/// 戻り値は送信したカタログの JSON バイト列である。MOQT relay が subscriber の
/// FETCH を転送してきた場合、publisher は同じバイト列を FETCH 応答として返すため、
/// 呼び出し側で保持する。
/// draft-ietf-moq-msf-01 §5 (Catalog)
pub async fn send_catalog(params: CatalogParams<'_>) -> Result<Vec<u8>> {
    let catalog = build_catalog(params.video.as_ref(), params.audio.as_ref(), params.sync)?;

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
        params.start_location,
    )
    .await?;
    let empty_props = LocProperties::new();
    let outcome = writer.write_object(&catalog_json, &empty_props).await?;
    if outcome == ObjectFilterOutcome::Skip {
        // 全 Skip の writer を reset で終端してから、カタログが届かないことをエラーとして報告する
        // (カタログがフィルタ不通過で届かないと subscriber は起動できないため、静かに飲み込まない)
        writer.finish(params.start_location)?;
        return Err(Error::Other(
            "catalog object skipped by subscription filter".to_string(),
        ));
    }
    writer.finish(params.start_location)?;

    Ok(catalog_json)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// テストで使う同期用メタデータ
    ///
    /// 既定のターゲットレイテンシ (200 ms) と同じ値を使う。
    const TEST_SYNC: CatalogSyncParams = CatalogSyncParams {
        render_group: 1,
        target_latency_ms: 200,
    };

    /// publisher が生成する video / audio の構成のカタログが encode に成功すること
    ///
    /// codec 文字列が audio / video と判定されること自体は、library 側の登録名テストで
    /// `av01.0.08M.08` / `opus` を含めて固定している。
    /// draft-ietf-moq-msf-01 §5.2.18 (Codec) / §5.2.22 (Maximum Bitrate)
    #[test]
    fn publisher_catalog_tracks_encode() {
        let video = VideoTrackParams {
            track_name: "video",
            namespace: "ns",
            codec: "av01.0.08M.08",
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
        };
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: "opus",
            samplerate: 48_000,
            channel_config: "2",
            bitrate: 128,
        };
        let catalog =
            build_catalog(Some(&video), Some(&audio), TEST_SYNC).expect("カタログを構築できること");
        catalog
            .encode()
            .expect("publisher のカタログが encode に成功すること");
    }

    /// 音声と映像の renderGroup / targetLatency が同一値で載り、encode / decode を往復できること
    ///
    /// 受信側は同じ render group のトラックを同時に再生し、targetLatency ぶん遅らせて表示する。
    /// draft-ietf-moq-msf-01 §5.2.8 (Target latency) は同じ render group のトラックに同一の
    /// targetLatency を MUST で求める。§5.2.11 (Render group) は同じ値のトラックを
    /// 同時に再生する SHOULD を定める。
    #[test]
    fn publisher_catalog_sets_sync_metadata_on_both_tracks() {
        let video = VideoTrackParams {
            track_name: "video",
            namespace: "ns",
            codec: "av01.0.08M.08",
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
        };
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: "opus",
            samplerate: 48_000,
            channel_config: "1",
            bitrate: 64,
        };
        let catalog =
            build_catalog(Some(&video), Some(&audio), TEST_SYNC).expect("カタログを構築できること");
        let encoded = catalog
            .encode()
            .expect("publisher のカタログが encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded).expect("encode したカタログを decode できること");
        let MsfCatalogDocument::Full(full) = decoded else {
            panic!("publisher は Full カタログを送ること");
        };
        assert_eq!(full.tracks.len(), 2, "音声と映像の 2 トラックを持つこと");

        let video_track = &full.tracks[0];
        let audio_track = &full.tracks[1];
        assert_eq!(
            video_track.render_group,
            Some(TEST_SYNC.render_group),
            "映像に renderGroup が載ること"
        );
        assert_eq!(
            audio_track.render_group,
            Some(TEST_SYNC.render_group),
            "音声に renderGroup が載ること"
        );
        assert_eq!(
            video_track.render_group, audio_track.render_group,
            "音声と映像の renderGroup が同一値であること"
        );
        assert_eq!(
            video_track.target_latency,
            Some(TEST_SYNC.target_latency_ms),
            "映像に targetLatency が載ること"
        );
        assert_eq!(
            audio_track.target_latency,
            Some(TEST_SYNC.target_latency_ms),
            "音声に targetLatency が載ること"
        );
        assert_eq!(
            video_track.target_latency, audio_track.target_latency,
            "音声と映像の targetLatency が同一値であること"
        );
    }

    /// publisher が載せる role が §5.2.6 Table 4 の予約 role であること
    ///
    /// 受信側は codec だけでなく role からも content の種別を判定できる。
    /// draft-ietf-moq-msf-01 §5.2.6 (Track role)
    #[test]
    fn publisher_catalog_sets_reserved_track_roles() {
        let video = VideoTrackParams {
            track_name: "video",
            namespace: "ns",
            codec: "av01.0.08M.08",
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
        };
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: "opus",
            samplerate: 48_000,
            channel_config: "2",
            bitrate: 128,
        };
        let catalog =
            build_catalog(Some(&video), Some(&audio), TEST_SYNC).expect("カタログを構築できること");
        let encoded = catalog
            .encode()
            .expect("publisher のカタログが encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded).expect("encode したカタログを decode できること");
        let MsfCatalogDocument::Full(full) = decoded else {
            panic!("publisher は Full カタログを送ること");
        };
        assert_eq!(
            full.tracks[0].role.as_deref(),
            Some(MSF_ROLE_VIDEO),
            "映像トラックに role 'video' が載ること"
        );
        assert_eq!(
            full.tracks[1].role.as_deref(),
            Some(MSF_ROLE_AUDIO),
            "音声トラックに role 'audio' が載ること"
        );
        // 映像だけ・音声だけのカタログでも role が載ること
        let video_only =
            build_catalog(Some(&video), None, TEST_SYNC).expect("カタログを構築できること");
        let MsfCatalogDocument::Full(full) = video_only
            .encode()
            .and_then(|encoded| MsfCatalogDocument::decode(&encoded))
            .expect("encode / decode に成功すること")
        else {
            panic!("publisher は Full カタログを送ること");
        };
        assert_eq!(
            full.tracks[0].role.as_deref(),
            Some(MSF_ROLE_VIDEO),
            "映像のみのカタログにも role が載ること"
        );
        let audio_only =
            build_catalog(None, Some(&audio), TEST_SYNC).expect("カタログを構築できること");
        let MsfCatalogDocument::Full(full) = audio_only
            .encode()
            .and_then(|encoded| MsfCatalogDocument::decode(&encoded))
            .expect("encode / decode に成功すること")
        else {
            panic!("publisher は Full カタログを送ること");
        };
        assert_eq!(
            full.tracks[0].role.as_deref(),
            Some(MSF_ROLE_AUDIO),
            "音声のみのカタログにも role が載ること"
        );
    }

    /// `--video-codec h264` / `h265` の codec 文字列でも encode に成功すること
    ///
    /// `avc1.640028` / `hvc1.1.6.L120.B0` が video と判定されることは、library 側の
    /// 登録名テストで固定している。
    #[test]
    fn publisher_alternate_video_codecs_encode() {
        for codec in ["avc1.640028", "hvc1.1.6.L120.B0"] {
            let video = VideoTrackParams {
                track_name: "video",
                namespace: "ns",
                codec,
                width: 1920,
                height: 1080,
                fps: 30,
                bitrate: 5_000,
            };
            let catalog =
                build_catalog(Some(&video), None, TEST_SYNC).expect("カタログを構築できること");
            catalog
                .encode()
                .expect("代替の video codec でも encode に成功すること");
        }
    }

    /// トラックが 1 つも無い場合はカタログを構築しないこと
    #[test]
    fn empty_catalog_is_rejected() {
        assert!(
            build_catalog(None, None, TEST_SYNC).is_err(),
            "トラックが 1 つも無い場合はエラーになること"
        );
    }
}

//! MSF カタログの生成と送信
//!
//! video / audio track の情報から MSF カタログを組み立て、catalog track に送信する。
//! draft-ietf-moq-msf-01 §5 (Catalog) に準拠する。

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message_parameter::{LocationFilter, LocationFilterContext};
use shiguredo_moqt::{
    msf::MSF_VERSION, msf::MsfAuthInfo, msf::MsfCatalog, msf::MsfCatalogDocument,
    msf::MsfPackaging, msf::MsfTrack,
};

use crate::error::{Error, Result};
use crate::stream_writer::SubgroupWriter;
use tokio_moq::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use tokio_moq::transport;

/// 映像トラックの role (draft-ietf-moq-msf-01 §5.2.6 (Track role) Table 4)
const MSF_ROLE_VIDEO: &str = "video";

/// 音声トラックの role (draft-ietf-moq-msf-01 §5.2.6 (Track role) Table 4)
const MSF_ROLE_AUDIO: &str = "audio";

/// C4M (CAT) の認可 scheme 名 (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info) Table 7)
const MSF_AUTH_SCHEME_CAT: &str = "cat";

/// 視聴側の URI の予約 fragment パラメータ `c4m` を指す変数参照
///
/// draft-ietf-moq-msf-01 §5.2.43 (Token Delivery via URI) / §5.4 (Variable Substitution)。
/// `authInfo` の値は scheme 固有の JSON 値であるため、JSON 文字列として持つ。
const C4M_AUTH_INFO_VALUE: &str = "\"%c4m%\"";

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
    /// カタログ最初の Object を載せる Group ID
    ///
    /// draft-ietf-moq-msf-01 §6.1 (Group numbering): Track の Group ID は一意で単調増加が
    /// MUST であり、publisher が再起動したときは以前に publish したどの Group ID よりも
    /// 大きい値から始めなければならない。Group ID を 0 に固定してはならない。
    pub group_id: u64,
    /// カタログ track の購読の Start Location (`MoqtClient::subscription_filter_start`)
    ///
    /// カタログは 1 Subgroup = 1 Object なので終端方法の判定結果は変わらない (省略があれば
    /// `SUBGROUP_HEADER` 未送信で RESET、配送すれば FIN)。複数 Object を持つ Subgroup と同じ
    /// 形で呼び出し側から値を渡しておく。
    pub start_location: Option<Location>,
    /// 音声・映像で共通の同期用メタデータ
    pub sync: CatalogSyncParams,
    /// URL の MSF fragment の `c4m` から取り出した C4M 認可トークン
    ///
    /// SETUP の AUTHORIZATION TOKEN (0x03) として送るトークンであり、空でないときは
    /// 映像 / 音声の track に authInfo を載せる (draft-ietf-moq-msf-01 §5.2.42)。
    pub c4m_tokens: &'a [Vec<u8>],
    pub video: Option<VideoTrackParams<'a>>,
    pub audio: Option<AudioTrackParams<'a>>,
}

/// SETUP に載せる C4M 認可トークンから、catalog の track に載せる authInfo を決める
///
/// draft-ietf-moq-msf-01 §5.2.42 (Authorization Info) / §11.4.1 (Discovering Authorization
/// Requirements): authInfo の存在は「この track の購読には認可トークンが必要」という
/// 視聴側へのシグナルであり、視聴側はこれを見てトークンの提示を決める。
///
/// - C4M トークン (Token Type CAT) が指定されているときだけ `{"cat": "%c4m%"}`
/// - 値はトークンそのものではなく、視聴側の URI の fragment の `c4m` を指す変数参照にする
///   (§5.2.43)。catalog は全ての視聴者に届き、配信者のトークンは PUBLISH の権限を含むため
/// - トークンが無いときは `None` (authInfo を載せない)
///
/// 本 example は `c4m` のトークンを常に Token Type CAT (0x01) として SETUP に載せるため
/// (`tokio_moq::build_setup_options`)、ここでは有無だけを見る。
fn auth_info_for_c4m_tokens(c4m_tokens: &[Vec<u8>]) -> Option<Vec<MsfAuthInfo>> {
    if c4m_tokens.is_empty() {
        return None;
    }
    Some(vec![MsfAuthInfo {
        scheme: MSF_AUTH_SCHEME_CAT.to_string(),
        value_raw: C4M_AUTH_INFO_VALUE.as_bytes().to_vec(),
    }])
}

/// video / audio のパラメータから MSF カタログを構築する
///
/// video / audio のいずれも指定されない場合は、トラックを 1 つも持たないカタログになるため
/// 構築せずにエラーを返す。
/// `auth_info` は track 単位のフィールドのため、載せるときは全ての track に同じ値を載せる
/// (draft-ietf-moq-msf-01 §5.2.42 (Authorization Info))。
/// draft-ietf-moq-msf-01 §5 (Catalog)
fn build_catalog(
    video: Option<&VideoTrackParams<'_>>,
    audio: Option<&AudioTrackParams<'_>>,
    sync: CatalogSyncParams,
    auth_info: Option<&[MsfAuthInfo]>,
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
        track.auth_info = auth_info.map(|infos| infos.to_vec());
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
        track.auth_info = auth_info.map(|infos| infos.to_vec());
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
/// カタログトラックとして `params.group_id` の Object 0 に Full カタログを送信する。
/// 戻り値は送信したカタログの JSON バイト列である。MOQT relay が subscriber の
/// FETCH を転送してきた場合、publisher は同じバイト列を FETCH 応答として返すため、
/// 呼び出し側で保持する。
/// C4M 認可トークンで接続する配信は、映像 / 音声の track に authInfo を載せ、
/// 視聴側に同じ方式のトークンの提示を求める (draft-ietf-moq-msf-01 §5.2.42)。
/// draft-ietf-moq-msf-01 §5 (Catalog)
pub async fn send_catalog(params: CatalogParams<'_>) -> Result<Vec<u8>> {
    let auth_info = auth_info_for_c4m_tokens(params.c4m_tokens);
    let catalog = build_catalog(
        params.video.as_ref(),
        params.audio.as_ref(),
        params.sync,
        auth_info.as_deref(),
    )?;

    let catalog_json = catalog
        .encode()
        .map_err(|e| Error::Other(format!("catalog encode: {e}")))?;
    tracing::info!(
        "Sending MSF catalog ({} bytes): {}",
        catalog_json.len(),
        String::from_utf8_lossy(&catalog_json)
    );

    publish_catalog_object(
        params.handle,
        params.data_plane,
        params.catalog_request_id,
        params.catalog_alias,
        params.group_id,
        &catalog_json,
        params.start_location,
    )
    .await
    .and_then(|outcome| {
        if outcome == ObjectFilterOutcome::Skip {
            // カタログがフィルタ不通過で届かないと subscriber は起動できないため、
            // 静かに飲み込まずエラーとして報告する
            Err(Error::Other(
                "catalog object skipped by subscription filter".to_string(),
            ))
        } else {
            Ok(())
        }
    })?;

    Ok(catalog_json)
}

/// MSF カタログの JSON を 1 Object (Object ID 0) として Group に送信する
///
/// draft-ietf-moq-msf-01 §5 (Catalog): 各 Group の最初の Object (Object ID 0) は独立した
/// 完全なカタログでなければならず、独立した更新は新しい Group の先頭に置く。カタログの
/// 送り直しは新しい Group ID でこの関数を呼ぶ。
/// draft-ietf-moq-msf-01 §5 (Catalog) は「配信網の cache から落ちうる時間が過ぎたら
/// publish し直す」を SHOULD とするため、呼び出し側が間隔を決める。
///
/// 戻り値は購読フィルタの評価結果である。`ObjectFilterOutcome::Skip` はどの購読にも
/// 届かなかったことを示す (draft-ietf-moq-transport-22 §3.3.1 (Location Filters) /
/// §3.3.3 (Combining Filters))。送り直しでは Skip を正常系として扱い、同じ Location を
/// 再送しないために Group ID だけを進める。
pub async fn publish_catalog_object(
    handle: &transport::StreamHandle,
    data_plane: &DataPlaneHandle,
    catalog_request_id: u64,
    catalog_alias: u64,
    group_id: u64,
    catalog_json: &[u8],
    start_location: Option<Location>,
) -> Result<ObjectFilterOutcome> {
    // カタログを Subgroup Stream として送信する。Subgroup ID は 0 でなければならない
    // (draft-ietf-moq-msf-01 §5: すべてのカタログ更新は MOQT sub-group 0 に置く MUST)
    let mut writer = SubgroupWriter::new(
        handle,
        data_plane,
        catalog_request_id,
        catalog_alias,
        group_id,
        128,
        false,
        start_location,
    )
    .await?;
    let empty_props = LocProperties::new();
    let outcome = writer.write_object(catalog_json, &empty_props).await?;
    writer.finish(start_location)?;
    Ok(outcome)
}

/// FETCH に応答するときの方針
///
/// draft-ietf-moq-transport-22 §3.2 (Fetch) / §3.2.1 (Fetch Object Delivery) /
/// §3.3.1 (Location Filters) / §9.20.9 (LOCATION FILTER Parameter) に基づく。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFetchResponse {
    /// 要求 range にカタログの Object が含まれる。FETCH_OK の End Location は
    /// カタログ自身の Location になり、Object を 1 つ返す
    Object { end_location: Location },
    /// 要求 range にカタログの Object が含まれない。FETCH_OK の End Location は
    /// 要求 range の終端になり、Object を返さずに FIN する
    Empty { end_location: Location },
    /// 開始 Location が Largest Object を超える、または range が空である。
    /// REQUEST_ERROR (INVALID_RANGE) を返す
    InvalidRange,
}

/// FETCH の Location Filter とカタログの Location から応答方針を決める
///
/// - フィルタ省略時は `{0, 0}` から Largest Object までを要求されたものとして扱う
///   (draft-ietf-moq-transport-22 §3.2)
/// - 開始 Location が Largest Object を超える場合は INVALID_RANGE が MUST (§3.2)
/// - range に Object が無い場合は空の FETCH 応答を返す (§3.2.1)
/// - 要求 range の外の Object を送ってはならない (§3.3.1)
///
/// `catalog` はこの publisher が publish 済みのカタログの Location であり、この track の
/// Largest Object でもある。
pub fn catalog_fetch_response(
    filter: Option<&LocationFilter>,
    catalog: Location,
) -> CatalogFetchResponse {
    // フィルタ省略時は既定の start `{0, 0}` と End = Largest Object を使う (§3.2)
    let default_start = Location {
        group_id: 0,
        object_id: 0,
    };
    let start = match filter {
        None => default_start,
        Some(filter) => match filter.effective_start_location(Some(&catalog)) {
            Some(start) => start,
            None => return CatalogFetchResponse::InvalidRange,
        },
    };
    let end = match filter {
        None => catalog,
        Some(filter) => filter
            .effective_end_location(Some(&catalog), LocationFilterContext::Fetch)
            .unwrap_or(catalog),
    };
    // 開始 Location が Largest Object を超える、または range が空の場合は INVALID_RANGE
    if start > catalog || start > end {
        return CatalogFetchResponse::InvalidRange;
    }
    // カタログが要求 range に含まれるときだけ Object を返す
    if start <= catalog && catalog <= end {
        return CatalogFetchResponse::Object {
            end_location: catalog,
        };
    }
    CatalogFetchResponse::Empty { end_location: end }
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
        let catalog = build_catalog(Some(&video), Some(&audio), TEST_SYNC, None)
            .expect("カタログを構築できること");
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
        let catalog = build_catalog(Some(&video), Some(&audio), TEST_SYNC, None)
            .expect("カタログを構築できること");
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
        let catalog = build_catalog(Some(&video), Some(&audio), TEST_SYNC, None)
            .expect("カタログを構築できること");
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
            build_catalog(Some(&video), None, TEST_SYNC, None).expect("カタログを構築できること");
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
            build_catalog(None, Some(&audio), TEST_SYNC, None).expect("カタログを構築できること");
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
            let catalog = build_catalog(Some(&video), None, TEST_SYNC, None)
                .expect("カタログを構築できること");
            catalog
                .encode()
                .expect("代替の video codec でも encode に成功すること");
        }
    }

    /// トラックが 1 つも無い場合はカタログを構築しないこと
    #[test]
    fn empty_catalog_is_rejected() {
        assert!(
            build_catalog(None, None, TEST_SYNC, None).is_err(),
            "トラックが 1 つも無い場合はエラーになること"
        );
    }

    /// C4M トークンがあるときだけ CAT の authInfo を組み立てること
    ///
    /// 値はトークンそのものではなく、視聴側の URI の `c4m` を指す変数参照にする
    /// (draft-ietf-moq-msf-01 §5.2.42 / §5.2.43)。
    #[test]
    fn auth_info_is_built_only_for_c4m_tokens() {
        assert!(
            auth_info_for_c4m_tokens(&[]).is_none(),
            "c4m が無ければ authInfo を載せない"
        );
        let auth_info = auth_info_for_c4m_tokens(&[vec![0x01, 0x02]])
            .expect("c4m があれば authInfo を組み立てられること");
        assert_eq!(auth_info.len(), 1, "CAT の 1 エントリを持つこと");
        assert_eq!(
            auth_info[0].scheme, MSF_AUTH_SCHEME_CAT,
            "scheme は §5.2.42 Table 7 の cat であること"
        );
        assert_eq!(
            auth_info[0].value_raw,
            C4M_AUTH_INFO_VALUE.as_bytes().to_vec(),
            "値は c4m を指す変数参照の JSON 文字列であること"
        );
    }

    /// C4M トークンで接続する配信が映像 / 音声の両トラックに authInfo を載せること
    ///
    /// 視聴側は authInfo の存在で track の認可要否を判断する (draft-ietf-moq-msf-01
    /// §5.2.42 / §11.4.1)。authInfo は track 単位のフィールドのため両トラックに載せる。
    #[test]
    fn publisher_catalog_sets_auth_info_on_both_tracks_for_c4m() {
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

        // c4m が無い場合は authInfo を載せない
        let catalog = build_catalog(Some(&video), Some(&audio), TEST_SYNC, None)
            .expect("カタログを構築できること");
        let MsfCatalogDocument::Full(full) = catalog else {
            panic!("publisher は Full カタログを送ること");
        };
        assert!(
            full.tracks.iter().all(|track| track.auth_info.is_none()),
            "c4m が無いときは authInfo を載せない"
        );

        // c4m がある場合は両トラックに載せ、encode / decode を往復しても保たれる
        let auth_info =
            auth_info_for_c4m_tokens(&[vec![0x01, 0x02]]).expect("authInfo を組み立てる");
        let catalog = build_catalog(
            Some(&video),
            Some(&audio),
            TEST_SYNC,
            Some(auth_info.as_slice()),
        )
        .expect("カタログを構築できること");
        let encoded = catalog
            .encode()
            .expect("publisher のカタログが encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded).expect("encode したカタログを decode できること");
        let MsfCatalogDocument::Full(full) = decoded else {
            panic!("publisher は Full カタログを送ること");
        };
        assert_eq!(full.tracks.len(), 2, "音声と映像の 2 トラックを持つこと");
        for track in &full.tracks {
            let track_auth_info = track
                .auth_info
                .as_ref()
                .unwrap_or_else(|| panic!("track '{}' に authInfo が載ること", track.name));
            assert_eq!(
                track_auth_info.as_slice(),
                auth_info.as_slice(),
                "track '{}' の authInfo が組み立てた値と一致すること",
                track.name
            );
        }
        // 配信者のトークンそのものではなく、視聴側の `c4m` を指す変数参照が JSON に載ること
        let text = String::from_utf8(encoded).expect("カタログは UTF-8 の JSON であること");
        assert!(
            text.contains(r#""authInfo":{"cat":"%c4m%"}"#),
            "catalog の JSON に authInfo が載ること: {text}"
        );
    }

    /// フィルタ省略の FETCH は `{0, 0}` から Largest Object までを要求したものとして扱うこと
    ///
    /// draft-ietf-moq-transport-22 §3.2 (Fetch): フィルタ省略時の range は `{0, 0}` と
    /// Largest Object。カタログがその範囲にあるため Object を返す。
    #[test]
    fn catalog_fetch_without_filter_serves_the_object() {
        let catalog = Location {
            group_id: 1_760_000_000_000,
            object_id: 0,
        };
        assert_eq!(
            catalog_fetch_response(None, catalog),
            CatalogFetchResponse::Object {
                end_location: catalog
            }
        );
    }

    /// カタログを含む絶対 range の FETCH は Object を返すこと
    ///
    /// draft-ietf-moq-transport-22 §3.5.1 (Dynamically Starting New Groups) の
    /// "observe the Largest Object in the response" パターンが要求する範囲である。
    #[test]
    fn catalog_fetch_with_range_containing_the_catalog_serves_the_object() {
        let catalog = Location {
            group_id: 1_760_000_000_000,
            object_id: 0,
        };
        let filter = LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: catalog.group_id,
                object_id: 0,
            },
            end_group_delta: 0,
            end_object: 0,
        };
        assert_eq!(
            catalog_fetch_response(Some(&filter), catalog),
            CatalogFetchResponse::Object {
                end_location: catalog
            }
        );
    }

    /// range の外にある FETCH は Object を返さず、要求 range の終端で空応答にすること
    ///
    /// draft-ietf-moq-transport-22 §3.2.1 (Fetch Object Delivery): 要求 range に Object が
    /// 無いときは uni stream を開いて FETCH_HEADER だけを送り FIN する。§3.3.1
    /// (Location Filters): publisher は要求 range の外の Object を送ってはならない。
    /// Group ID を 0 に固定して要求する古い形の FETCH がこれに該当する。
    #[test]
    fn catalog_fetch_outside_the_range_is_empty() {
        let catalog = Location {
            group_id: 1_760_000_000_000,
            object_id: 0,
        };
        let filter = LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: 0,
                object_id: 0,
            },
            end_group_delta: 0,
            end_object: 1,
        };
        assert_eq!(
            catalog_fetch_response(Some(&filter), catalog),
            CatalogFetchResponse::Empty {
                end_location: Location {
                    group_id: 0,
                    object_id: 1,
                }
            }
        );
    }

    /// 開始 Location が Largest Object を超える FETCH は INVALID_RANGE にすること
    ///
    /// draft-ietf-moq-transport-22 §3.2 (Fetch): "If no Objects have been published for the
    /// track or Start Location is greater than the Largest Object the publisher MUST return
    /// FETCH_ERROR with error code INVALID_RANGE."
    #[test]
    fn catalog_fetch_starting_after_the_catalog_is_invalid_range() {
        let catalog = Location {
            group_id: 1_760_000_000_000,
            object_id: 0,
        };
        let filter = LocationFilter::AbsoluteStart {
            start: Location {
                group_id: catalog.group_id + 1,
                object_id: 0,
            },
        };
        assert_eq!(
            catalog_fetch_response(Some(&filter), catalog),
            CatalogFetchResponse::InvalidRange
        );
    }

    /// 絶対 range の終端がカタログより手前の FETCH は空応答にすること
    ///
    /// 開始 Location は Largest Object 以下だが、終端がカタログに届かない範囲である。
    #[test]
    fn catalog_fetch_ending_before_the_catalog_is_empty() {
        let catalog = Location {
            group_id: 1_760_000_000_000,
            object_id: 0,
        };
        let filter = LocationFilter::AbsoluteRangeWithEnd {
            start: Location {
                group_id: 1_759_999_999_999,
                object_id: 0,
            },
            end_group_delta: 0,
            end_object: 0,
        };
        assert_eq!(
            catalog_fetch_response(Some(&filter), catalog),
            CatalogFetchResponse::Empty {
                end_location: Location {
                    group_id: 1_759_999_999_999,
                    object_id: 0,
                }
            }
        );
    }
}

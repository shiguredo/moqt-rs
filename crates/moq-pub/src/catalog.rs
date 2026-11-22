//! MSF カタログの生成と送信
//!
//! video / audio track の情報から MSF カタログを組み立て、catalog track に送信する。
//! draft-ietf-moq-msf-02 §5 (Catalog) に準拠する。

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message_parameter::{LocationFilter, LocationFilterContext};
use shiguredo_moqt::stream::subgroup::SubgroupIdMode;
use shiguredo_moqt::{
    msf::MSF_VERSION, msf::MsfAuthInfo, msf::MsfCatalog, msf::MsfCatalogDocument,
    msf::MsfPackaging, msf::MsfTrack, msf::StreamMapping,
};

use crate::error::{Error, Result};
use crate::stream_writer::SubgroupWriter;
use tokio_moq::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use tokio_moq::transport;

/// 映像トラックの role (draft-ietf-moq-msf-02 §5.2.7 (Track role) Table 10)
const MSF_ROLE_VIDEO: &str = "video";

/// 音声トラックの role (draft-ietf-moq-msf-02 §5.2.7 (Track role) Table 10)
const MSF_ROLE_AUDIO: &str = "audio";

/// C4M (CAT) の認可 scheme 名 (draft-ietf-moq-msf-02 §5.2.42 (Authorization Info) Table 10)
const MSF_AUTH_SCHEME_CAT: &str = "cat";

/// 視聴側の URI の予約 fragment パラメータ `cat` を指す変数参照
///
/// draft-ietf-moq-msf-02 §5.2.43 (Token Delivery via URI) / §5.4 (Variable Substitution)。
/// `authInfo` の値は scheme 固有の JSON 値であるため、JSON 文字列として持つ。
/// `%cat%` の end-to-end な解決は、moq-sub に変数置換の呼び出し経路が無いため本実装の
/// 範囲外である (catalog は未解決の参照を載せたまま送る)。
const CAT_AUTH_INFO_VALUE: &str = "\"%cat%\"";

/// 映像トラックの catalog 情報
pub struct VideoTrackParams<'a> {
    pub track_name: &'a str,
    pub namespace: &'a str,
    /// RFC 6381 形式の codec 文字列 (例: `av01.0.08M.08`)
    pub codec: &'a str,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// 単位は kbps。
    pub bitrate: u32,
    /// 映像トラックの最大 Group 長 (ms)
    ///
    /// `maxGopDuration` と `maxGroupDuration` の両方に同じ値を載せる。この publisher は
    /// Group を 1 キーフレーム間隔で進めるため、Group = 1 GOP が成り立つ
    /// (draft-ietf-moq-msf-02 §5.2.25 (Maximum GOP Duration) / §5.2.26 (Maximum Group
    /// Duration))。Group 長を宣言しない経路 (MP4 パススルーは MP4 のキーフレームで
    /// Group が進む) では `None` とし、どちらのフィールドも載せない。
    pub max_group_duration_ms: Option<u64>,
    /// LOC packaging の Object を MOQT Stream へ割り当てるモード
    ///
    /// draft-ietf-moq-msf-02 §4.1.1 (LOC to MOQT Stream mapping) / §5.2.5 (Packaging
    /// metadata): `packagingMetadata` の `streamMapping` キーで per-track に通知する。
    pub stream_mapping: StreamMapping,
}

/// 音声トラックの catalog 情報
pub struct AudioTrackParams<'a> {
    pub track_name: &'a str,
    pub namespace: &'a str,
    /// RFC 6381 形式の codec 文字列 (例: `opus`)
    pub codec: &'a str,
    /// サンプリングレート Hz
    pub samplerate: u32,
    /// MSF の channelConfig (mono = "1" / stereo = "2")
    /// draft-ietf-moq-msf-02 §5.2.30 (Channel configuration)
    pub channel_config: &'a str,
    /// 単位は kbps。
    pub bitrate: u32,
}

/// publisher が音声・映像の両トラックへ共通で載せる同期用のメタデータ
///
/// 音声と映像を同じ時間軸で再生するため、両トラックへ同じ値を載せる。
/// draft-ietf-moq-msf-02 §5.2.9 (Target latency) は同じ render group のトラックに
/// 同一の targetLatency を MUST で求める。
#[derive(Debug, Clone, Copy)]
pub struct CatalogSyncParams {
    /// レンダーグループ
    ///
    /// 同じ値を持つトラックは同時に再生する (draft-ietf-moq-msf-02 §5.2.12)。
    pub render_group: u64,
    /// ターゲットレイテンシ (ms)
    ///
    /// 符号化した時刻から表示するまでのずれ (draft-ietf-moq-msf-02 §5.2.9)。
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
    /// draft-ietf-moq-msf-02 §6.1 (Group numbering): Track の Group ID は一意で単調増加が
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
    /// URL の MSF fragment の `cat` から取り出した C4M 認可トークン
    ///
    /// SETUP の AUTHORIZATION TOKEN (0x03) として送るトークンであり、空でないときは
    /// 映像 / 音声の track に authInfo を載せる (draft-ietf-moq-msf-02 §5.2.42)。
    pub cat_tokens: &'a [Vec<u8>],
    pub video: Option<VideoTrackParams<'a>>,
    pub audio: Option<AudioTrackParams<'a>>,
}

/// SETUP に載せる C4M 認可トークンから、catalog の track に載せる authInfo を決める
///
/// draft-ietf-moq-msf-02 §5.2.42 (Authorization Info) / §11.5.1 (Discovering Authorization
/// Requirements): authInfo の存在は「この track の購読には認可トークンが必要」という
/// 視聴側へのシグナルであり、視聴側はこれを見てトークンの提示を決める。
///
/// - C4M トークン (Token Type CAT) が指定されているときだけ `{"cat": "%cat%"}`
/// - 値はトークンそのものではなく、視聴側の URI の fragment の `cat` を指す変数参照にする
///   (§5.2.43)。catalog は全ての視聴者に届き、配信者のトークンは PUBLISH の権限を含むため
/// - トークンが無いときは `None` (authInfo を載せない)
///
/// 本 example は `cat` のトークンを常に Token Type CAT (0x01) として SETUP に載せるため
/// (`tokio_moq::build_setup_options`)、ここでは有無だけを見る。
fn auth_info_for_cat_tokens(cat_tokens: &[Vec<u8>]) -> Option<Vec<MsfAuthInfo>> {
    if cat_tokens.is_empty() {
        return None;
    }
    Some(vec![MsfAuthInfo {
        scheme: MSF_AUTH_SCHEME_CAT.to_string(),
        value_raw: CAT_AUTH_INFO_VALUE.as_bytes().to_vec(),
    }])
}

/// video / audio のパラメータから MSF カタログを構築する
///
/// video / audio のいずれも指定されない場合は、トラックを 1 つも持たないカタログになるため
/// 構築せずにエラーを返す。
/// `auth_info` は track 単位のフィールドのため、載せるときは全ての track に同じ値を載せる
/// (draft-ietf-moq-msf-02 §5.2.42 (Authorization Info))。
/// draft-ietf-moq-msf-02 §5 (Catalog)
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
        // draft-ietf-moq-msf-02 §5.2.7 (Track role) Table 10 の予約 role を載せる。
        // 受信側は codec だけでなく role からも content の種別を判定できる
        track.role = Some(MSF_ROLE_VIDEO.to_string());
        track.codec = Some(v.codec.to_string());
        track.width = Some(v.width as u64);
        track.height = Some(v.height as u64);
        track.framerate = Some(v.fps as f64);
        track.bitrate = Some(v.bitrate as u64 * 1000);
        // Group 長の宣言。Group = 1 GOP のため maxGopDuration (§5.2.24) と
        // maxGroupDuration (§5.2.25) に同じ値を載せる。購読者はバッファ量の見積もりに使う
        track.max_gop_duration = v.max_group_duration_ms;
        track.max_group_duration = v.max_group_duration_ms;
        track.target_latency = Some(sync.target_latency_ms);
        track.render_group = Some(sync.render_group);
        track.auth_info = auth_info.map(|infos| infos.to_vec());
        // draft-ietf-moq-msf-02 §4.1.1 / §5.2.5: LOC packaging の Object から MOQT Stream
        // への割り当てモードを per-track に通知する。キーが無い場合は `group` と仮定される
        // ため、`group` でも publisher が選んだモードを明示する
        track.packaging_metadata = Some(format!("streamMapping={}", v.stream_mapping.as_str()));
        tracks.push(track);
    }

    if let Some(a) = audio {
        let mut track = MsfTrack::new(a.track_name.to_string(), MsfPackaging::Loc, true);
        track.namespace = Some(a.namespace.to_string());
        // draft-ietf-moq-msf-02 §5.2.7 (Track role) Table 10 の予約 role を載せる。
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

/// MSF カタログの JSON を組み立てる
///
/// track の authInfo は渡した C4M 認可トークンから作るため、GOAWAY 移行で移行先の `cat` が
/// 変わったときは新しいトークンで呼び直す
/// (draft-ietf-moq-msf-02 §5.2.42 (Authorization Info) / §5.2.43 (Token Delivery via URI))。
/// 送信は行わない (送信は [`send_catalog`] / [`publish_catalog_object`] が担う)。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
/// draft-ietf-moq-msf-02 §5 (Catalog)
pub fn build_catalog_json(
    video: Option<&VideoTrackParams<'_>>,
    audio: Option<&AudioTrackParams<'_>>,
    sync: CatalogSyncParams,
    cat_tokens: &[Vec<u8>],
) -> Result<Vec<u8>> {
    let auth_info = auth_info_for_cat_tokens(cat_tokens);
    let catalog = build_catalog(video, audio, sync, auth_info.as_deref())?;
    catalog
        .encode(shiguredo_moqt::msf::MsfCompression::None)
        .map_err(|e| Error::Other(format!("catalog encode: {e}")))
}

/// MSF カタログを構築して送信する
///
/// カタログトラックとして `params.group_id` の Object 0 に Full カタログを送信する。
/// 戻り値は送信したカタログの JSON バイト列である。MOQT relay が subscriber の
/// FETCH を転送してきた場合、publisher は同じバイト列を FETCH 応答として返すため、
/// 呼び出し側で保持する。
/// C4M 認可トークンで接続する配信は、映像 / 音声の track に authInfo を載せ、
/// 視聴側に同じ方式のトークンの提示を求める (draft-ietf-moq-msf-02 §5.2.42)。
/// draft-ietf-moq-msf-02 §5 (Catalog)
pub async fn send_catalog(params: CatalogParams<'_>) -> Result<Vec<u8>> {
    // 組み立ては `build_catalog_json` に任せ、移行後の再構築と同じ経路を通す
    let catalog_json = build_catalog_json(
        params.video.as_ref(),
        params.audio.as_ref(),
        params.sync,
        params.cat_tokens,
    )?;
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

/// カタログトラックの publisher priority
///
/// draft-ietf-moq-msf-02 Appendix A.1.3: カタログトラックは記述する track より高い
/// publisher priority を持つこと (SHOULD)。MOQT §5.1 は値が小さいほど優先度が高いため、
/// media の既定値 128 より小さい 64 を使う。カタログの全送信経路がこの定数を参照する。
pub(crate) const CATALOG_PUBLISHER_PRIORITY: u8 = 64;

/// MSF カタログの JSON を 1 Object (Object ID 0) として Group に送信する
///
/// draft-ietf-moq-msf-02 §5 (Catalog): 各 Group の最初の Object (Object ID 0) は独立した
/// 完全なカタログでなければならず、独立した更新は新しい Group の先頭に置く。カタログの
/// 送り直しは新しい Group ID でこの関数を呼ぶ。
/// draft-ietf-moq-msf-02 §5 (Catalog) は「配信網の cache から落ちうる時間が過ぎたら
/// publish し直す」を SHOULD とするため、呼び出し側が間隔を決める。
///
/// 戻り値は購読フィルタの評価結果である。`ObjectFilterOutcome::Skip` はどの購読にも
/// 届かなかったことを示す (draft-ietf-moq-transport-23 §3.3.1 (Location Filters) /
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
    // (draft-ietf-moq-msf-02 §5: すべてのカタログ更新は MOQT sub-group 0 に置く MUST)
    let mut writer = SubgroupWriter::new(
        handle,
        data_plane,
        catalog_request_id,
        catalog_alias,
        group_id,
        SubgroupIdMode::Zero,
        0,
        CATALOG_PUBLISHER_PRIORITY,
        false,
        start_location,
    )
    .await?;
    let empty_props = LocProperties::new();
    let outcome = writer.write_object(catalog_json, &empty_props).await?;
    writer.finish(start_location).await?;
    Ok(outcome)
}

/// FETCH に応答するときの方針
///
/// draft-ietf-moq-transport-23 §3.2 (Fetch) / §3.2.1 (Fetch Object Delivery) /
/// §3.3.1 (Location Filters) / §9.19.9 (LOCATION FILTER Parameter) に基づく。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogFetchResponse {
    /// 要求 range にカタログの Object が含まれる。FETCH_OK の応答範囲の終端は
    /// カタログ自身の Location (この track の Largest Object) になり、Object を 1 つ返す
    Object { end_location: Location },
    /// 要求 range にカタログの Object が含まれない。FETCH_OK の応答範囲の終端は
    /// 要求 range の終端になり、Object を返さずに FIN する
    Empty { end_location: Location },
    /// 開始 Location が Largest Object を超える、または range が空である。
    /// REQUEST_ERROR (INVALID_RANGE) を返す
    InvalidRange,
}

/// FETCH の Location Filter とカタログの Location から応答方針を決める
///
/// - フィルタ省略時は `{0, 0}` から Largest Object までを要求されたものとして扱う
///   (draft-ietf-moq-transport-23 §3.2)
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
    use crate::encoder::av1::AV1_CATALOG_CODEC_STRING;
    #[cfg(target_os = "macos")]
    use crate::encoder::h264::DEFAULT_AVC_CATALOG_CODEC_STRING;
    #[cfg(target_os = "macos")]
    use crate::encoder::h265::DEFAULT_HEVC_CATALOG_CODEC_STRING;
    use crate::encoder::opus::OPUS_CATALOG_CODEC_STRING;

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
    /// draft-ietf-moq-msf-02 §5.2.19 (Codec) / §5.2.23 (Maximum Bitrate)
    #[test]
    fn publisher_catalog_tracks_encode() {
        let video = VideoTrackParams {
            track_name: "video",
            namespace: "ns",
            codec: AV1_CATALOG_CODEC_STRING,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
            max_group_duration_ms: None,
            stream_mapping: StreamMapping::Group,
        };
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: OPUS_CATALOG_CODEC_STRING,
            samplerate: 48_000,
            channel_config: "2",
            bitrate: 128,
        };
        let catalog = build_catalog(Some(&video), Some(&audio), TEST_SYNC, None)
            .expect("カタログを構築できること");
        catalog
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .expect("publisher のカタログが encode に成功すること");
    }

    /// 音声と映像の renderGroup / targetLatency が同一値で載り、encode / decode を往復できること
    ///
    /// 受信側は同じ render group のトラックを同時に再生し、targetLatency ぶん遅らせて表示する。
    /// draft-ietf-moq-msf-02 §5.2.9 (Target latency) は同じ render group のトラックに同一の
    /// targetLatency を MUST で求める。§5.2.11 (Render group) は同じ値のトラックを
    /// 同時に再生する SHOULD を定める。
    #[test]
    fn publisher_catalog_sets_sync_metadata_on_both_tracks() {
        let video = VideoTrackParams {
            track_name: "video",
            namespace: "ns",
            codec: AV1_CATALOG_CODEC_STRING,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
            max_group_duration_ms: None,
            stream_mapping: StreamMapping::Group,
        };
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: OPUS_CATALOG_CODEC_STRING,
            samplerate: 48_000,
            channel_config: "1",
            bitrate: 64,
        };
        let catalog = build_catalog(Some(&video), Some(&audio), TEST_SYNC, None)
            .expect("カタログを構築できること");
        let encoded = catalog
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .expect("publisher のカタログが encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
                .expect("encode したカタログを decode できること");
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

    /// 映像トラックの packagingMetadata に選択した streamMapping を載せること
    ///
    /// draft-ietf-moq-msf-02 §4.1.1 (LOC to MOQT Stream mapping) / §5.2.5 (Packaging
    /// metadata): publisher が選んだモードを per-track に通知する。キーが無い場合は
    /// `group` と仮定されるため、`group` でも明示する。
    #[test]
    fn catalog_announces_the_video_stream_mapping() {
        for (mapping, expected) in [
            (StreamMapping::Group, "streamMapping=group"),
            (StreamMapping::Object, "streamMapping=object"),
        ] {
            let video = VideoTrackParams {
                track_name: "video",
                namespace: "ns",
                codec: AV1_CATALOG_CODEC_STRING,
                width: 1920,
                height: 1080,
                fps: 30,
                bitrate: 5_000,
                max_group_duration_ms: None,
                stream_mapping: mapping,
            };
            let catalog = build_catalog(Some(&video), None, TEST_SYNC, None)
                .expect("カタログを構築できること");
            let MsfCatalogDocument::Full(full) = catalog else {
                panic!("publisher は Full カタログを送ること");
            };
            let track = &full.tracks[0];
            assert_eq!(
                track.packaging_metadata.as_deref(),
                Some(expected),
                "video track の packagingMetadata に streamMapping を載せること: {mapping:?}"
            );
            assert_eq!(
                track.stream_mapping(),
                Ok(mapping),
                "載せた値を購読側の accessor で読み戻せること"
            );
        }
    }

    /// 映像トラックに maxGopDuration / maxGroupDuration を載せること
    ///
    /// Group は 1 キーフレーム間隔 = 1 GOP であるため、両方に同じ値を載せる。値は
    /// 時間ベースの判定が境界の 1 フレームぶん遅れ得ることを見込んだ Group 長 + 1 フレーム
    /// (30 fps / 2000 ms なら 2034 ms) である。Group 長を宣言できない経路ではどちらも
    /// 載せない。draft-ietf-moq-msf-02 §5.2.25 (Maximum GOP Duration) /
    /// §5.2.25 (Maximum Group Duration)
    #[test]
    fn publisher_catalog_sets_max_gop_and_group_duration() {
        let mut video = VideoTrackParams {
            track_name: "video",
            namespace: "ns",
            codec: AV1_CATALOG_CODEC_STRING,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
            max_group_duration_ms: Some(2_034),
            stream_mapping: StreamMapping::Group,
        };
        let catalog =
            build_catalog(Some(&video), None, TEST_SYNC, None).expect("カタログを構築できること");
        let encoded = catalog
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .expect("publisher のカタログが encode に成功すること");
        let MsfCatalogDocument::Full(full) =
            MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
                .expect("encode したカタログを decode できること")
        else {
            panic!("publisher は Full カタログを送ること");
        };
        assert_eq!(
            full.tracks[0].max_gop_duration,
            Some(2_034),
            "maxGopDuration を載せること"
        );
        assert_eq!(
            full.tracks[0].max_group_duration,
            Some(2_034),
            "maxGroupDuration に同じ値を載せること"
        );

        // Group 長を宣言できない経路 (MP4 パススルー) ではどちらも載せない
        video.max_group_duration_ms = None;
        let catalog =
            build_catalog(Some(&video), None, TEST_SYNC, None).expect("カタログを構築できること");
        let encoded = catalog
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .expect("publisher のカタログが encode に成功すること");
        let MsfCatalogDocument::Full(full) =
            MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
                .expect("encode したカタログを decode できること")
        else {
            panic!("publisher は Full カタログを送ること");
        };
        assert_eq!(
            full.tracks[0].max_gop_duration, None,
            "宣言しない場合は maxGopDuration を載せないこと"
        );
        assert_eq!(
            full.tracks[0].max_group_duration, None,
            "宣言しない場合は maxGroupDuration を載せないこと"
        );
    }

    /// `--audio-codec aac` の codec 文字列でも encode に成功すること
    ///
    /// `mp4a.40.2` が audio と判定されることは、library 側の登録名テストで固定している。
    /// AAC-LC の channelConfiguration は MSF の channelConfig と同じ値 (mono = 1 / stereo = 2) である。
    #[test]
    fn publisher_aac_audio_codec_encodes() {
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: "mp4a.40.2",
            samplerate: 48_000,
            channel_config: "1",
            bitrate: 64,
        };
        let catalog =
            build_catalog(None, Some(&audio), TEST_SYNC, None).expect("カタログを構築できること");
        catalog
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .expect("AAC-LC の audio codec でも encode に成功すること");
    }

    /// publisher が載せる role が §5.2.6 Table 4 の予約 role であること
    ///
    /// 受信側は codec だけでなく role からも content の種別を判定できる。
    /// draft-ietf-moq-msf-02 §5.2.7 (Track role)
    #[test]
    fn publisher_catalog_sets_reserved_track_roles() {
        let video = VideoTrackParams {
            track_name: "video",
            namespace: "ns",
            codec: AV1_CATALOG_CODEC_STRING,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
            max_group_duration_ms: None,
            stream_mapping: StreamMapping::Group,
        };
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: OPUS_CATALOG_CODEC_STRING,
            samplerate: 48_000,
            channel_config: "2",
            bitrate: 128,
        };
        let catalog = build_catalog(Some(&video), Some(&audio), TEST_SYNC, None)
            .expect("カタログを構築できること");
        let encoded = catalog
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .expect("publisher のカタログが encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
                .expect("encode したカタログを decode できること");
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
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .and_then(|encoded| {
                MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
            })
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
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .and_then(|encoded| {
                MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
            })
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

    /// `--video-codec h264` / `h265` / `vp8` / `vp9` の codec 文字列でも encode に成功すること
    ///
    /// `avc1.640028` / `hvc1.1.6.L120.B0` / `vp8` / `vp09.00.40.08` が video と判定されることは、
    /// library 側の登録名テストで固定している。
    #[cfg(target_os = "macos")]
    #[test]
    fn publisher_alternate_video_codecs_encode() {
        for codec in [
            DEFAULT_AVC_CATALOG_CODEC_STRING,
            DEFAULT_HEVC_CATALOG_CODEC_STRING,
            "vp8",
            "vp09.00.40.08",
        ] {
            let video = VideoTrackParams {
                track_name: "video",
                namespace: "ns",
                codec,
                width: 1920,
                height: 1080,
                fps: 30,
                bitrate: 5_000,
                max_group_duration_ms: None,
                stream_mapping: StreamMapping::Group,
            };
            let catalog = build_catalog(Some(&video), None, TEST_SYNC, None)
                .expect("カタログを構築できること");
            catalog
                .encode(shiguredo_moqt::msf::MsfCompression::None)
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
    /// 値はトークンそのものではなく、視聴側の URI の `cat` を指す変数参照にする
    /// (draft-ietf-moq-msf-02 §5.2.42 / §5.2.43)。
    #[test]
    fn auth_info_is_built_only_for_cat_tokens() {
        assert!(
            auth_info_for_cat_tokens(&[]).is_none(),
            "cat が無ければ authInfo を載せない"
        );
        let auth_info = auth_info_for_cat_tokens(&[vec![0x01, 0x02]])
            .expect("cat があれば authInfo を組み立てられること");
        assert_eq!(auth_info.len(), 1, "CAT の 1 エントリを持つこと");
        assert_eq!(
            auth_info[0].scheme, MSF_AUTH_SCHEME_CAT,
            "scheme は §5.2.42 Table 7 の cat であること"
        );
        assert_eq!(
            auth_info[0].value_raw,
            CAT_AUTH_INFO_VALUE.as_bytes().to_vec(),
            "値は cat を指す変数参照の JSON 文字列であること"
        );
    }

    /// カタログ JSON の組み立てが C4M トークンの有無で authInfo を出し分けること
    ///
    /// 移行先の cat が変わったときに、同じ track 情報から authInfo を載せ替えた JSON を
    /// 組み立てられることを固定する (draft-ietf-moq-msf-02 §5.2.42 (Authorization Info))。
    #[test]
    fn build_catalog_json_reflects_cat_tokens() {
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: OPUS_CATALOG_CODEC_STRING,
            samplerate: 48_000,
            channel_config: "1",
            bitrate: 64,
        };

        // cat が無ければ authInfo を載せない
        let without_tokens = build_catalog_json(None, Some(&audio), TEST_SYNC, &[])
            .expect("cat 無しでもカタログを組み立てられること");
        let decoded =
            MsfCatalogDocument::decode(&without_tokens, shiguredo_moqt::msf::MsfCompression::None)
                .expect("組み立てたカタログを decode できること");
        let MsfCatalogDocument::Full(full) = decoded else {
            panic!("publisher は Full カタログを組み立てること");
        };
        assert!(
            full.tracks.iter().all(|track| track.auth_info.is_none()),
            "cat が無いときは authInfo を載せないこと"
        );

        // cat があれば authInfo を載せる
        let tokens = vec![b"publisher-token".to_vec()];
        let with_tokens = build_catalog_json(None, Some(&audio), TEST_SYNC, &tokens)
            .expect("cat 有りでもカタログを組み立てられること");
        let decoded =
            MsfCatalogDocument::decode(&with_tokens, shiguredo_moqt::msf::MsfCompression::None)
                .expect("組み立てたカタログを decode できること");
        let MsfCatalogDocument::Full(full) = decoded else {
            panic!("publisher は Full カタログを組み立てること");
        };
        let track_auth_info = full.tracks[0]
            .auth_info
            .as_ref()
            .expect("cat があるときは authInfo を載せること");
        assert_eq!(
            track_auth_info[0].scheme, MSF_AUTH_SCHEME_CAT,
            "authInfo の scheme は §5.2.42 Table 7 の cat であること"
        );

        // 配信者のトークンそのものは JSON に載せず、視聴側の cat を指す変数参照にする
        let text = String::from_utf8(with_tokens).expect("カタログは UTF-8 の JSON であること");
        assert!(
            !text.contains("publisher-token"),
            "C4M トークンの値をカタログへ埋め込まないこと: {text}"
        );
        assert!(
            text.contains(r#""authInfo":{"cat":"%cat%"}"#),
            "authInfo は cat を指す変数参照であること: {text}"
        );
    }

    /// C4M トークンで接続する配信が映像 / 音声の両トラックに authInfo を載せること
    ///
    /// 視聴側は authInfo の存在で track の認可要否を判断する (draft-ietf-moq-msf-02
    /// §5.2.42 / §11.4.1)。authInfo は track 単位のフィールドのため両トラックに載せる。
    #[test]
    fn publisher_catalog_sets_auth_info_on_both_tracks_for_c4m() {
        let video = VideoTrackParams {
            track_name: "video",
            namespace: "ns",
            codec: AV1_CATALOG_CODEC_STRING,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
            max_group_duration_ms: None,
            stream_mapping: StreamMapping::Group,
        };
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "ns",
            codec: OPUS_CATALOG_CODEC_STRING,
            samplerate: 48_000,
            channel_config: "2",
            bitrate: 128,
        };

        // cat が無い場合は authInfo を載せない
        let catalog = build_catalog(Some(&video), Some(&audio), TEST_SYNC, None)
            .expect("カタログを構築できること");
        let MsfCatalogDocument::Full(full) = catalog else {
            panic!("publisher は Full カタログを送ること");
        };
        assert!(
            full.tracks.iter().all(|track| track.auth_info.is_none()),
            "cat が無いときは authInfo を載せない"
        );

        // cat がある場合は両トラックに載せ、encode / decode を往復しても保たれる
        let auth_info =
            auth_info_for_cat_tokens(&[vec![0x01, 0x02]]).expect("authInfo を組み立てる");
        let catalog = build_catalog(
            Some(&video),
            Some(&audio),
            TEST_SYNC,
            Some(auth_info.as_slice()),
        )
        .expect("カタログを構築できること");
        let encoded = catalog
            .encode(shiguredo_moqt::msf::MsfCompression::None)
            .expect("publisher のカタログが encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded, shiguredo_moqt::msf::MsfCompression::None)
                .expect("encode したカタログを decode できること");
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
        // 配信者のトークンそのものではなく、視聴側の `cat` を指す変数参照が JSON に載ること
        let text = String::from_utf8(encoded).expect("カタログは UTF-8 の JSON であること");
        assert!(
            text.contains(r#""authInfo":{"cat":"%cat%"}"#),
            "catalog の JSON に authInfo が載ること: {text}"
        );
    }

    /// フィルタ省略の FETCH は `{0, 0}` から Largest Object までを要求したものとして扱うこと
    ///
    /// draft-ietf-moq-transport-23 §3.2 (Fetch): フィルタ省略時の range は `{0, 0}` と
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
    /// draft-ietf-moq-transport-23 §3.5.1 (Dynamically Starting New Groups) の
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
    /// draft-ietf-moq-transport-23 §3.2.1 (Fetch Object Delivery): 要求 range に Object が
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
    /// draft-ietf-moq-transport-23 §3.2 (Fetch): "If no Objects have been published for the
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

    /// カタログの namespace を v02 の `/` 区切りで出力すること
    ///
    /// draft-ietf-moq-msf-02 §5.2.2 (Track namespace): publisher は catalog track の
    /// namespace を `/` 区切りで書き、subscriber は同じ Track Namespace に解決する。
    /// moq-sub が使う `parse_catalog_namespace` で往復できることを固定する。
    #[test]
    fn publisher_catalog_writes_v02_namespace() {
        let video = VideoTrackParams {
            track_name: "video",
            namespace: "customer/livestream/123",
            codec: AV1_CATALOG_CODEC_STRING,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate: 5_000,
            max_group_duration_ms: None,
            stream_mapping: StreamMapping::Group,
        };
        let json = build_catalog_json(Some(&video), None, TEST_SYNC, &[])
            .expect("v02 の namespace でカタログを組み立てられること");
        let text = String::from_utf8(json.clone()).expect("JSON は UTF-8 であること");
        assert!(
            text.contains(r#""namespace":"customer/livestream/123""#),
            "namespace を '/' 区切りで出力すること: {text}"
        );

        let decoded = MsfCatalogDocument::decode(&json, shiguredo_moqt::msf::MsfCompression::None)
            .expect("テストフィクスチャの前提条件を満たす");
        let MsfCatalogDocument::Full(catalog) = decoded else {
            panic!("publisher は Full カタログを組み立てること");
        };
        let namespace = shiguredo_moqt::msf::parse_catalog_namespace(
            catalog.tracks[0]
                .namespace
                .as_deref()
                .expect("namespace が出力されること"),
        )
        .expect("subscriber 側で解決できること");
        assert_eq!(
            shiguredo_moqt::name::serialize_name(&namespace, b"video"),
            "customer-livestream-123--video",
            "subscriber が同じ Full Track Name に解決できること"
        );
    }

    /// カタログで表現できない namespace は encode で拒否すること
    #[test]
    fn publisher_catalog_rejects_unrepresentable_namespace() {
        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: "a//b",
            codec: OPUS_CATALOG_CODEC_STRING,
            samplerate: 48_000,
            channel_config: "1",
            bitrate: 64,
        };
        assert!(
            build_catalog_json(None, Some(&audio), TEST_SYNC, &[]).is_err(),
            "空の field を含む namespace をカタログに書かないこと"
        );
    }

    /// MOQT の Track Namespace から v02 の `/` 区切り表現を作る経路でもカタログを組み立てられること
    ///
    /// publisher の実行経路は `TrackNamespace` を `serialize_catalog_namespace` で変換し、
    /// その文字列を [`VideoTrackParams::namespace`] / [`AudioTrackParams::namespace`] へ渡す。
    /// 変換とカタログ組み立ての組み合わせを固定する。
    #[test]
    fn publisher_catalog_accepts_serialized_track_namespace() {
        let namespace = shiguredo_moqt::message::common::TrackNamespace::new(vec![
            "customer".as_bytes().to_vec(),
            "ライブ".as_bytes().to_vec(),
        ])
        .expect("テスト用 namespace を作れること");
        let namespace_text = shiguredo_moqt::msf::serialize_catalog_namespace(&namespace)
            .expect("カタログ形式に変換できること");
        assert_eq!(namespace_text, "customer/ライブ");

        let audio = AudioTrackParams {
            track_name: "audio",
            namespace: &namespace_text,
            codec: OPUS_CATALOG_CODEC_STRING,
            samplerate: 48_000,
            channel_config: "1",
            bitrate: 64,
        };
        let json = build_catalog_json(None, Some(&audio), TEST_SYNC, &[])
            .expect("変換した namespace でカタログを組み立てられること");
        let decoded = MsfCatalogDocument::decode(&json, shiguredo_moqt::msf::MsfCompression::None)
            .expect("テストフィクスチャの前提条件を満たす");
        let MsfCatalogDocument::Full(catalog) = decoded else {
            panic!("publisher は Full カタログを組み立てること");
        };
        assert_eq!(
            catalog.tracks[0].namespace.as_deref(),
            Some("customer/ライブ"),
            "Track Namespace の各 field が '/' 区切りで載ること"
        );
    }

    /// カタログの publisher priority が media の既定値 128 より高いこと
    #[test]
    fn catalog_publisher_priority_is_higher_than_media() {
        let priority = std::hint::black_box(CATALOG_PUBLISHER_PRIORITY);
        assert!(
            priority < 128,
            "カタログの priority は media の既定値より小さく (高く) すること"
        );
    }
}

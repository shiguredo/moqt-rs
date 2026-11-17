use pbt::common::test_runner;
use shiguredo_moqt::msf::{
    MSF_VERSION, MsfAccessibility, MsfAuthInfo, MsfBuffers, MsfCatalog, MsfCatalogDocument,
    MsfCloneTrack, MsfDeltaOperation, MsfDeltaUpdate, MsfDependency, MsfEventIndex,
    MsfEventTimeline, MsfEventTimelineEntry, MsfInitData, MsfInitDataKind, MsfMediaTimeline,
    MsfMediaTimelineEntry, MsfPackaging, MsfRemoveTrack, MsfTemplate, MsfTrack,
    TimelineEncodingOptions, decode_event_timeline, decode_media_timeline, encode_event_timeline,
    encode_media_timeline, parse_catalog_namespace, serialize_catalog_namespace,
};

// ─── 生成ヘルパー ─────────────────────────────────────────────────────────────

/// 有効な lang 文字列を生成する
///
/// draft-ietf-moq-msf-01 §5.2.32 (Language): BCP 47 言語タグ。
/// primary 2〜3 文字 + 任意個の `-` 区切り英数字サブタグを基本とし、
/// 1/8 の重みで RFC 5646 §2.1 (Syntax) の grandfathered タグ
/// (`i-klingon` のように primary subtag が 1 文字の irregular タグを含む) を返す。
/// None:Some = 3:2 の重みのため、全体に占める割合は約 1/20 (Some のうち 1/8)。
fn sample_lang(ctx: &mut noprop::TestCaseContext) -> Option<String> {
    if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 5)) {
        return None;
    }
    // grandfathered タグは langtag の規則に一致しないものがあるため、
    // 軽量検証 (src/msf.rs の validate_lang_tag) と encode / decode 往復を PBT で固定する
    if noprop::sample_ratio(ctx, noprop::Ratio::new(1, 8)) {
        return Some(
            noprop::sample_choice(
                ctx,
                &["i-klingon", "I-AMI", "i-default", "en-GB-oed", "zh-min-nan"],
            )
            .to_string(),
        );
    }
    let primary_len = noprop::sample_usize_in(ctx, 2..=3);
    let mut s: String = (0..primary_len)
        .map(|_| {
            noprop::sample_choice(ctx, b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ")
                as char
        })
        .collect();
    let subtag_count = noprop::sample_usize_in(ctx, 0..=2);
    for _ in 0..subtag_count {
        let sub_len = noprop::sample_usize_in(ctx, 1..=8);
        s.push('-');
        s.extend((0..sub_len).map(|_| {
            noprop::sample_choice(
                ctx,
                b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789",
            ) as char
        }));
    }
    Some(s)
}

/// 有効な ASCII 識別子文字列を生成する ([a-z][a-z0-9\-]{0,15})
fn sample_name(ctx: &mut noprop::TestCaseContext) -> String {
    let mut s = String::new();
    s.push(noprop::sample_choice(ctx, b"abcdefghijklmnopqrstuvwxyz") as char);
    let tail_len = noprop::sample_usize_in(ctx, 0..=15);
    s.extend(
        (0..tail_len)
            .map(|_| noprop::sample_choice(ctx, b"abcdefghijklmnopqrstuvwxyz0123456789-") as char),
    );
    s
}

/// MsfPackaging を生成する
///
/// draft-ietf-moq-msf-01 §5.2.4 (Packaging): 許容値は loc / mediatimeline / eventtimeline / moqlog / moqmetrics
fn sample_packaging(ctx: &mut noprop::TestCaseContext) -> MsfPackaging {
    noprop::sample_choice(
        ctx,
        &[
            MsfPackaging::Loc,
            MsfPackaging::MediaTimeline,
            MsfPackaging::EventTimeline,
            MsfPackaging::MoqLog,
            MsfPackaging::MoqMetrics,
            MsfPackaging::Catalog,
        ],
    )
}

/// None/Some を 1:1 で生成する
fn sample_optional_u32(ctx: &mut noprop::TestCaseContext) -> Option<u32> {
    if noprop::sample_bool(ctx) {
        Some(noprop::sample_u32(ctx))
    } else {
        None
    }
}

/// MsfBuffers を生成する (None:Some = 3:1 の重み)
fn sample_buffers(ctx: &mut noprop::TestCaseContext) -> Option<MsfBuffers> {
    if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
        return None;
    }
    Some(MsfBuffers {
        target: sample_optional_u32(ctx).map(|v| v as u64),
        min: sample_optional_u32(ctx).map(|v| v as u64),
        max: sample_optional_u32(ctx).map(|v| v as u64),
    })
}

/// packagingMetadata を生成する (None:Some = 1:1)
///
/// draft-ietf-moq-msf-02 §5.2.5 (Packaging metadata): `;` 区切りの `key=value`。
/// 既知の `streamMapping` と未知の key を混ぜ、先頭優先の解釈も往復させる。
fn sample_packaging_metadata(ctx: &mut noprop::TestCaseContext) -> Option<String> {
    if !noprop::sample_bool(ctx) {
        return None;
    }
    Some(
        noprop::sample_choice(
            ctx,
            &[
                "streamMapping=group",
                "streamMapping=object",
                "unknown=value;streamMapping=group",
                "a=b;c=d;a=first",
            ],
        )
        .to_string(),
    )
}

/// 通常トラック (tracks / publishTracks / addTracks 用) を生成する
///
/// - parentName は含めない (tracks/addTracks では禁止)
/// - isLive/targetLatency/trackDuration/buffers の相互制約を守る
/// - mediatimeline/eventtimeline は depends + mimeType 必須
fn sample_track(ctx: &mut noprop::TestCaseContext) -> MsfTrack {
    let name = sample_name(ctx);
    let packaging = sample_packaging(ctx);
    let event_type_val = sample_name(ctx); // eventType 用の値
    let is_live = noprop::sample_bool(ctx);
    let target_latency = sample_optional_u32(ctx);
    let buffers = sample_buffers(ctx);
    let track_duration = sample_optional_u32(ctx);
    let render_group = sample_optional_u32(ctx);
    let bitrate = sample_optional_u32(ctx);
    let avg_bitrate = sample_optional_u32(ctx);
    let max_gop_duration = sample_optional_u32(ctx);
    let max_group_duration = sample_optional_u32(ctx);
    let depends_count = noprop::sample_usize_in(ctx, 0..=3);
    let depends_raw = (0..depends_count)
        .map(|_| MsfDependency::TrackName(sample_name(ctx)))
        .collect::<Vec<_>>();
    let lang = sample_lang(ctx);

    let mut t = MsfTrack::new(name, packaging, is_live);
    t.packaging_metadata = sample_packaging_metadata(ctx);
    // v02 の `/` 区切り namespace を一定確率で明示し、継承と明示の両方を往復させる
    if noprop::sample_bool(ctx) {
        t.namespace = Some(sample_catalog_namespace(ctx));
    }

    // eventtimeline なら eventType 必須
    if matches!(packaging, MsfPackaging::EventTimeline) {
        t.event_type = Some(event_type_val);
    }

    // mediatimeline / eventtimeline は depends 必須 + mimeType=application/json 必須
    let is_timeline = matches!(
        packaging,
        MsfPackaging::MediaTimeline | MsfPackaging::EventTimeline
    );
    if is_timeline {
        t.depends = if depends_raw.is_empty() {
            vec![MsfDependency::TrackName("default".to_string())]
        } else {
            depends_raw
        };
        t.mime_type = Some("application/json".to_string());
    } else {
        t.depends = depends_raw;
    }
    t.template = sample_template(ctx);

    // 生成器は正規形 (isLive=false なら None) を作る。encode 側も decode の正規化に合わせるため
    // (draft-ietf-moq-msf-01 §5.2.8 / §5.2.9 は受信側の無視規則)、roundtrip が成立する
    t.target_latency = if is_live {
        target_latency.map(|v| v as u64)
    } else {
        None
    };
    t.buffers = if is_live { buffers } else { None };
    // isLive=true なら trackDuration 禁止
    t.track_duration = if is_live {
        None
    } else {
        track_duration.map(|v| v as u64)
    };
    // isLive=true なら targetLatency と buffers は相互排他
    if t.target_latency.is_some() {
        t.buffers = None;
    }

    t.render_group = render_group.map(|v| v as u64);
    t.bitrate = bitrate.map(|v| v as u64);
    t.avg_bitrate = avg_bitrate.map(|v| v as u64);
    t.max_gop_duration = max_gop_duration.map(|v| v as u64);
    t.max_group_duration = max_group_duration.map(|v| v as u64);
    // connectionURI は任意文字列を受理するため、現実形状 (URI) の生成にする
    // (JSON エスケープ要文字の頑健性は sample_catalog_namespace の field で担保)
    t.connection_uri = if noprop::sample_bool(ctx) {
        Some(sample_uri_like(ctx))
    } else {
        None
    };
    t.auth_info = sample_auth_info(ctx);
    t.accessibility = sample_accessibility(ctx);
    // 暗号化 signaling は充足形でのみ生成する
    // (encryptionScheme ありの欠如形は decode reject のため。cipherSuite 単独等の orphan は受理される)
    if noprop::sample_bool(ctx) {
        t.encryption_scheme = Some("moq-secure-objects".to_string());
        t.cipher_suite = Some("aes-128-gcm-sha256".to_string());
        t.key_id = Some("key-1".to_string());
        t.track_base_key = Some("AAEC".to_string());
    }
    t.lang = lang;
    t
}

/// draft-ietf-moq-msf-02 §5.2.2 の制約を満たす namespace 文字列を生成する
///
/// 1 〜 3 個の非空 field を `/` で連結する。field が空にならず 32 fields を超えないため、
/// `parse_catalog_namespace` が受理する。JSON エスケープの往復も検証できるよう、ときどき
/// 引用符・backslash・制御文字・非 ASCII を含む field を混ぜる (NUL と `/` は §5.2.2 /
/// §11.2.1 が禁止するため含めない)。
fn sample_catalog_namespace(ctx: &mut noprop::TestCaseContext) -> String {
    let count = noprop::sample_usize_in(ctx, 1..=3);
    let fields = (0..count)
        .map(|_| {
            if noprop::sample_ratio(ctx, noprop::Ratio::new(1, 4)) {
                noprop::sample_choice(
                    ctx,
                    &[
                        "café",
                        "a\u{feff}b",
                        "日本語",
                        "a\"b",
                        "a\\b",
                        "a\nb",
                        "a\u{1}b",
                    ],
                )
                .to_string()
            } else {
                sample_name(ctx)
            }
        })
        .collect::<Vec<_>>();
    fields.join("/")
}

/// clone operation 用トラックを生成する (parentName 必須)
///
/// draft-ietf-moq-msf-01 §5.1.6 (Delta update): clone track は親の属性を継承し、
/// 再定義された属性のみ上書きする。packaging/isLive は省略可。
fn sample_clone_track(ctx: &mut noprop::TestCaseContext) -> MsfCloneTrack {
    let name = sample_name(ctx);
    let parent = sample_name(ctx);
    let include_pkg = noprop::sample_bool(ctx);
    let pkg = sample_packaging(ctx);
    let include_live = noprop::sample_bool(ctx);
    let is_live = noprop::sample_bool(ctx);
    let bitrate = sample_optional_u32(ctx);
    let avg_bitrate = sample_optional_u32(ctx);
    let max_gop_duration = sample_optional_u32(ctx);
    let max_group_duration = sample_optional_u32(ctx);
    let width = sample_optional_u32(ctx);
    let lang = sample_lang(ctx);
    // 暗号化 signaling は充足形と cipherSuite 欠如の代表形を生成する
    // (clone は欠如 reject を行わないため受理される。代表形のみで十分)
    let encrypted = noprop::sample_bool(ctx);
    let partial = noprop::sample_bool(ctx);
    // parent_namespace も draft-ietf-moq-msf-02 §5.2.2 の `/` 区切り namespace 制約を
    // 満たす必要があるため、有効な namespace だけを生成する
    let parent_namespace = if noprop::sample_bool(ctx) {
        Some(sample_catalog_namespace(ctx))
    } else {
        None
    };

    MsfCloneTrack {
        name,
        parent_name: parent,
        parent_namespace,
        packaging: if include_pkg { Some(pkg) } else { None },
        packaging_metadata: sample_packaging_metadata(ctx),
        is_live: if include_live { Some(is_live) } else { None },
        namespace: None,
        event_type: None,
        role: None,
        target_latency: None,
        buffers: None,
        label: None,
        render_group: None,
        alt_group: None,
        init_ref: None,
        depends: None,
        template: sample_template(ctx),
        temporal_id: None,
        spatial_id: None,
        codec: None,
        mime_type: None,
        framerate: None,
        timescale: None,
        bitrate: bitrate.map(|v| v as u64),
        avg_bitrate: avg_bitrate.map(|v| v as u64),
        max_gop_duration: max_gop_duration.map(|v| v as u64),
        max_group_duration: max_group_duration.map(|v| v as u64),
        width: width.map(|v| v as u64),
        height: None,
        samplerate: None,
        channel_config: None,
        display_width: None,
        display_height: None,
        lang,
        track_duration: None,
        connection_uri: if noprop::sample_bool(ctx) {
            Some(sample_uri_like(ctx))
        } else {
            None
        },
        encryption_scheme: if encrypted {
            Some("moq-secure-objects".to_string())
        } else {
            None
        },
        // partial 時は cipherSuite のみ欠如させる (clone は欠如 reject しないため受理される)
        cipher_suite: if encrypted && !partial {
            Some("aes-128-gcm-sha256".to_string())
        } else {
            None
        },
        key_id: if encrypted {
            Some("key-1".to_string())
        } else {
            None
        },
        track_base_key: if encrypted {
            Some("AAEC".to_string())
        } else {
            None
        },
        auth_info: sample_auth_info(ctx),
        accessibility: if noprop::sample_bool(ctx) {
            Some(sample_accessibility(ctx))
        } else {
            None
        },
    }
}

/// URI らしい文字列を生成する (`:` / `/` / `.` を含む)
fn sample_uri_like(ctx: &mut noprop::TestCaseContext) -> String {
    let host = sample_name(ctx);
    let port = noprop::sample_u32(ctx) % 65536;
    format!("moqt://{host}.example.com:{port}")
}

/// authInfo を生成する (None:Some = 3:1 の重み。値は string / object / 数値等を混ぜる)
fn sample_auth_info(ctx: &mut noprop::TestCaseContext) -> Option<Vec<MsfAuthInfo>> {
    if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
        return None;
    }
    // scheme は登録名・ RDNN 形・生成名を混ぜる
    fn sample_scheme(ctx: &mut noprop::TestCaseContext) -> String {
        match noprop::sample_usize_in(ctx, 0..=2) {
            0 => "cat".to_string(),
            1 => "com.example.custom-auth".to_string(),
            _ => sample_name(ctx),
        }
    }
    let count = noprop::sample_usize_in(ctx, 1..=2);
    let entries = (0..count)
        .map(|_| {
            // string / object / 数値 / 真偽値 / null / 配列 / エスケープ付きを混ぜる
            let value_raw = match noprop::sample_usize_in(ctx, 0..=6) {
                0 => b"\"x\"".to_vec(),
                1 => b"{}".to_vec(),
                2 => b"123".to_vec(),
                3 => b"true".to_vec(),
                4 => b"null".to_vec(),
                5 => b"[1,\"a\"]".to_vec(),
                _ => b"{\"token\":\"%id%\"}".to_vec(),
            };
            MsfAuthInfo {
                scheme: sample_scheme(ctx),
                value_raw,
            }
        })
        .collect();
    Some(entries)
}

/// accessibility 記述子列を生成する (空の場合あり。608 / 708 の実形状を混ぜる)
fn sample_accessibility(ctx: &mut noprop::TestCaseContext) -> Vec<MsfAccessibility> {
    // scheme は実 URN と生成名を混ぜる。値は実形状 2 種を混ぜる
    fn sample_scheme(ctx: &mut noprop::TestCaseContext) -> String {
        match noprop::sample_usize_in(ctx, 0..=2) {
            0 => "urn:scte:dash:cc:cea-608:2015".to_string(),
            1 => "urn:scte:dash:cc:cea-708:2015".to_string(),
            _ => sample_name(ctx),
        }
    }
    fn sample_access_value(ctx: &mut noprop::TestCaseContext) -> String {
        match noprop::sample_usize_in(ctx, 0..=2) {
            0 => "CC1=eng;CC3=spa".to_string(),
            1 => "1=lang:eng;2=lang:spa;3=lang:fra".to_string(),
            _ => "a\"b\\c".to_string(),
        }
    }
    let count = noprop::sample_usize_in(ctx, 0..=2);
    (0..count)
        .map(|_| MsfAccessibility {
            scheme: sample_scheme(ctx),
            value: sample_access_value(ctx),
        })
        .collect()
}

/// MsfTemplate を生成する (None:Some = 3:1 の重み)
fn sample_template(ctx: &mut noprop::TestCaseContext) -> Option<MsfTemplate> {
    if noprop::sample_ratio(ctx, noprop::Ratio::new(3, 4)) {
        return None;
    }
    Some(MsfTemplate {
        start_media_time: noprop::sample_u32(ctx) as u64,
        delta_media_time: noprop::sample_u32(ctx) as u64,
        start_group_id: noprop::sample_u32(ctx) as u64,
        start_object_id: noprop::sample_u32(ctx) as u64,
        delta_group_id: noprop::sample_u32(ctx) as u64,
        delta_object_id: noprop::sample_u32(ctx) as u64,
        start_wallclock: noprop::sample_u32(ctx) as u64,
        delta_wallclock: noprop::sample_u32(ctx) as u64,
    })
}

/// MsfInitDataKind を生成する
fn sample_init_data_kind(ctx: &mut noprop::TestCaseContext) -> MsfInitDataKind {
    noprop::sample_choice(ctx, &[MsfInitDataKind::Inline])
}

/// base64 ライクな文字列 ([A-Za-z0-9+/=] のみ) を生成する
fn sample_base64ish(ctx: &mut noprop::TestCaseContext) -> String {
    let len = noprop::sample_usize_in(ctx, 0..=16);
    (0..len)
        .map(|_| {
            noprop::sample_choice(
                ctx,
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=",
            ) as char
        })
        .collect()
}

/// MsfInitData を生成する
fn sample_init_data(ctx: &mut noprop::TestCaseContext) -> MsfInitData {
    MsfInitData {
        id: sample_name(ctx),
        kind: sample_init_data_kind(ctx),
        data: sample_base64ish(ctx),
    }
}

/// initDataList を生成する (id の重複を除去する)
fn sample_init_data_list(ctx: &mut noprop::TestCaseContext) -> Vec<MsfInitData> {
    let count = noprop::sample_usize_in(ctx, 0..=3);
    let list = (0..count)
        .map(|_| sample_init_data(ctx))
        .collect::<Vec<_>>();
    let mut seen = std::collections::HashSet::new();
    list.into_iter()
        .filter(|e| seen.insert(e.id.clone()))
        .collect()
}

/// MsfRemoveTrack を生成する
fn sample_remove_track(ctx: &mut noprop::TestCaseContext) -> MsfRemoveTrack {
    MsfRemoveTrack {
        name: sample_name(ctx),
        namespace: None,
    }
}

/// MsfMediaTimelineEntry を生成する
fn sample_media_timeline_entry(ctx: &mut noprop::TestCaseContext) -> MsfMediaTimelineEntry {
    MsfMediaTimelineEntry {
        pts_ms: noprop::sample_u32(ctx) as u64,
        group_id: noprop::sample_u32(ctx) as u64,
        object_id: noprop::sample_u32(ctx) as u64,
        wallclock_ms: noprop::sample_u32(ctx) as u64,
    }
}

/// MsfEventIndex を生成する
fn sample_event_index(ctx: &mut noprop::TestCaseContext) -> MsfEventIndex {
    match noprop::sample_weighted_index(ctx, &[1, 1, 1]) {
        0 => MsfEventIndex::Location(
            noprop::sample_u32(ctx) as u64,
            noprop::sample_u32(ctx) as u64,
        ),
        1 => MsfEventIndex::WallclockMs(noprop::sample_u32(ctx) as u64),
        _ => MsfEventIndex::MediaPtsMs(noprop::sample_u32(ctx) as u64),
    }
}

/// MsfEventTimelineEntry を生成する
///
/// draft-ietf-moq-msf-01 §8.1 (Event Timeline data format): data は JSON Object でなければならない。
fn sample_event_timeline_entry(ctx: &mut noprop::TestCaseContext) -> MsfEventTimelineEntry {
    let index = sample_event_index(ctx);
    let data_raw = if noprop::sample_bool(ctx) {
        let v = noprop::sample_u32(ctx);
        format!(r#"{{"v":{v}}}"#).into_bytes()
    } else {
        b"{}".to_vec()
    };
    MsfEventTimelineEntry { index, data_raw }
}

// ─── PBT ─────────────────────────────────────────────────────────────────────

/// depends を、生成済みカタログ内で解決できる track name / renderGroup に置き換える
///
/// draft-ietf-moq-msf-02 §5.2.15 (Dependencies): track name は依存元以外の track に
/// ちょうど 1 つ一致し、renderGroup はカタログ内に存在しなければならない。PBT の生成器は
/// 独立に track を作るため、カタログ全体ができた後に参照先を実在する値へ差し替える。
/// timeline は depends 非空が必須のため、候補が無ければ専用の renderGroup を足す。
fn rewrite_dependencies(
    tracks: &mut [MsfTrack],
    publish_tracks: &mut [MsfTrack],
    ctx: &mut noprop::TestCaseContext,
) {
    let all_names = tracks
        .iter()
        .chain(publish_tracks.iter())
        .map(|track| track.name.clone())
        .collect::<Vec<_>>();
    let mut render_groups = tracks
        .iter()
        .chain(publish_tracks.iter())
        .filter_map(|track| track.render_group)
        .collect::<Vec<_>>();
    // media timeline が個別名を列挙した後に別の track へ renderGroup が付くと参照規則に
    // 反するため、renderGroup が 1 つも無ければ先頭 track に作っておく
    if render_groups.is_empty()
        && let Some(first) = tracks.first_mut()
    {
        first.render_group = Some(0);
        render_groups.push(0);
    }

    let rewrite_one = |source: &mut MsfTrack, ctx: &mut noprop::TestCaseContext| {
        let source_name = source.name.clone();
        let valid_names = all_names
            .iter()
            .filter(|name| {
                name.as_str() != source_name
                    && all_names
                        .iter()
                        .filter(|candidate| *candidate == *name)
                        .count()
                        == 1
            })
            .cloned()
            .collect::<Vec<_>>();

        let is_media_timeline = source.packaging == MsfPackaging::MediaTimeline;
        let is_timeline = is_media_timeline || source.packaging == MsfPackaging::EventTimeline;
        let count = noprop::sample_usize_in(ctx, if is_timeline { 1..=2 } else { 0..=2 });
        let mut depends = Vec::new();
        for _ in 0..count {
            // media timeline が renderGroup を参照するときは所属 track の個別名を併記しない
            // (draft-ietf-moq-msf-02 §7.2)。renderGroup がある場合はグループ参照だけにする。
            if is_media_timeline && !render_groups.is_empty() {
                depends.push(MsfDependency::RenderGroup(noprop::sample_choice(
                    ctx,
                    &render_groups,
                )));
            } else if !valid_names.is_empty()
                && (render_groups.is_empty() || noprop::sample_bool(ctx))
            {
                depends.push(MsfDependency::TrackName(
                    noprop::sample_choice(ctx, &valid_names).clone(),
                ));
            } else if !render_groups.is_empty() {
                depends.push(MsfDependency::RenderGroup(noprop::sample_choice(
                    ctx,
                    &render_groups,
                )));
            }
        }
        if is_timeline && depends.is_empty() {
            // 既存の renderGroup と衝突しない値を足し、そのグループへの依存にする
            let fresh = render_groups.iter().max().map_or(0, |max| max + 1);
            source.render_group = Some(fresh);
            depends.push(MsfDependency::RenderGroup(fresh));
        }
        source.depends = depends;
    };

    for track in tracks.iter_mut() {
        rewrite_one(track, ctx);
    }
    for track in publish_tracks.iter_mut() {
        rewrite_one(track, ctx);
    }
}

/// Full catalog の encode → decode ラウンドトリップ
///
/// - トラック名は catalog 通して (tracks / publishTracks) namespace ごとに一意でなければならない (draft-ietf-moq-msf-01 §5.2.3 (Track name))
/// - isComplete: false はエンコード時に省略され、デコードで false に戻る
#[test]
fn full_catalog_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let track_count = noprop::sample_usize_in(ctx, 0..=5);
        let tracks = (0..track_count)
            .map(|_| sample_track(ctx))
            .collect::<Vec<_>>();
        // トラック名の一意性を保証する (namespace, name) の組で重複排除
        let mut seen = std::collections::HashSet::new();
        let unique_tracks: Vec<_> = tracks
            .into_iter()
            .filter(|t| seen.insert((t.namespace.clone(), t.name.clone())))
            .collect();
        let init_data_list = sample_init_data_list(ctx);
        let generated_at = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u32(ctx) as u64)
        } else {
            None
        };

        // publish_tracks も tracks と同じ (namespace, name) 重複排除を適用する (seen を共有)
        let publish_count = noprop::sample_usize_in(ctx, 0..=2);
        let mut publish_tracks: Vec<_> = (0..publish_count)
            .map(|_| sample_track(ctx))
            .filter(|t| seen.insert((t.namespace.clone(), t.name.clone())))
            .collect();
        let mut unique_tracks = unique_tracks;
        rewrite_dependencies(&mut unique_tracks, &mut publish_tracks, ctx);
        let doc = MsfCatalogDocument::Full(MsfCatalog {
            version: MSF_VERSION.to_string(),
            generated_at,
            // isComplete: false はフィールド未出力 → デコード後も false なのでラウンドトリップ可
            is_complete: false,
            tracks: unique_tracks,
            publish_tracks,
            removed_tracks: Default::default(),
            init_data_list,
        });
        let encoded = doc.encode().expect("encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded, doc);
        Ok(())
    })?;
    Ok(())
}

/// isComplete: true の Full catalog ラウンドトリップ
#[test]
fn full_catalog_is_complete_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let generated_at = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u32(ctx) as u64)
        } else {
            None
        };
        let doc = MsfCatalogDocument::Full(MsfCatalog {
            version: MSF_VERSION.to_string(),
            generated_at,
            is_complete: true,
            tracks: vec![],
            publish_tracks: Vec::new(),
            removed_tracks: Default::default(),
            init_data_list: Vec::new(),
        });
        let encoded = doc.encode().expect("encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded, doc);
        Ok(())
    })?;
    Ok(())
}

/// Delta update の encode → decode ラウンドトリップ
///
/// add_tracks/remove_tracks/clone_tracks のうち少なくとも 1 つは非空にする
/// (draft-ietf-moq-msf-01 §5.3 (Delta updates))。
#[test]
fn delta_roundtrip() -> noprop::TestResult {
    // 3 操作すべてが空 (1/64 の確率) はデルタとして無効なため reject する
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let add_count = noprop::sample_usize_in(ctx, 0..=3);
        let remove_count = noprop::sample_usize_in(ctx, 0..=3);
        let clone_count = noprop::sample_usize_in(ctx, 0..=3);
        let add_tracks = (0..add_count)
            .map(|_| sample_track(ctx))
            .collect::<Vec<_>>();
        let remove_tracks = (0..remove_count)
            .map(|_| sample_remove_track(ctx))
            .collect::<Vec<_>>();
        let clone_tracks = (0..clone_count)
            .map(|_| sample_clone_track(ctx))
            .collect::<Vec<_>>();
        if add_tracks.is_empty() && remove_tracks.is_empty() && clone_tracks.is_empty() {
            ctx.reject_case();
        }
        let generated_at = if noprop::sample_bool(ctx) {
            Some(noprop::sample_u32(ctx) as u64)
        } else {
            None
        };
        // 操作を add → remove → clone の順で 1 つの operation object ずつ構築する
        let mut operations = Vec::new();
        if !add_tracks.is_empty() {
            operations.push(MsfDeltaOperation::Add { tracks: add_tracks });
        }
        if !remove_tracks.is_empty() {
            operations.push(MsfDeltaOperation::Remove {
                tracks: remove_tracks,
            });
        }
        if !clone_tracks.is_empty() {
            operations.push(MsfDeltaOperation::Clone {
                tracks: clone_tracks,
            });
        }
        let doc = MsfCatalogDocument::Delta(MsfDeltaUpdate {
            generated_at,
            operations,
        });
        let encoded = doc.encode().expect("encode に成功すること");
        let decoded =
            MsfCatalogDocument::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded, doc);
        Ok(())
    })?;
    Ok(())
}

/// MsfMediaTimeline の encode → decode ラウンドトリップ
#[test]
fn media_timeline_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let entry_count = noprop::sample_usize_in(ctx, 0..=20);
        let entries = (0..entry_count)
            .map(|_| sample_media_timeline_entry(ctx))
            .collect();
        let tl = MsfMediaTimeline(entries);
        let encoded = tl.encode();
        let decoded =
            MsfMediaTimeline::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded, tl);
        Ok(())
    })?;
    Ok(())
}

/// MsfEventTimeline の encode → decode ラウンドトリップ
#[test]
fn event_timeline_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let entry_count = noprop::sample_usize_in(ctx, 0..=20);
        let entries = (0..entry_count)
            .map(|_| sample_event_timeline_entry(ctx))
            .collect();
        let tl = MsfEventTimeline(entries);
        let encoded = tl.encode().expect("テストフィクスチャの前提条件を満たす");
        let decoded =
            MsfEventTimeline::decode(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded, tl);
        Ok(())
    })?;
    Ok(())
}

// 「encode 結果が UTF-8」は JSON encoder が String から構築される以上ほぼ自明で、
// 値の正しさは roundtrip 系が実際の一致で検証しているため、独立した property としては持たない。

/// MsfMediaTimeline の gzip ラウンドトリップ
#[test]
fn media_timeline_gzip_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let entry_count = noprop::sample_usize_in(ctx, 0..=20);
        let entries = (0..entry_count)
            .map(|_| sample_media_timeline_entry(ctx))
            .collect();
        let tl = MsfMediaTimeline(entries);
        let encoded = encode_media_timeline(&tl, TimelineEncodingOptions { gzip: true })
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(&encoded[..2], &[0x1F, 0x8B]);
        let decoded =
            decode_media_timeline(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded, tl);
        Ok(())
    })?;
    Ok(())
}

/// MsfEventTimeline の gzip ラウンドトリップ
#[test]
fn event_timeline_gzip_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let entry_count = noprop::sample_usize_in(ctx, 0..=20);
        let entries = (0..entry_count)
            .map(|_| sample_event_timeline_entry(ctx))
            .collect();
        let tl = MsfEventTimeline(entries);
        let encoded = encode_event_timeline(&tl, TimelineEncodingOptions { gzip: true })
            .expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(&encoded[..2], &[0x1F, 0x8B]);
        let decoded =
            decode_event_timeline(&encoded).expect("テストフィクスチャの前提条件を満たす");
        assert_eq!(decoded, tl);
        Ok(())
    })?;
    Ok(())
}

/// カタログ namespace の parse / serialize 往復
///
/// draft-ietf-moq-msf-02 §5.2.2 / §11.2: `/` 区切りの正規形を書いた文字列は、Track Namespace
/// に写像して戻すと同じ文字列になる。JSON エスケープを要する field も往復させる。
#[test]
fn catalog_namespace_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let value = sample_catalog_namespace(ctx);
        let namespace =
            parse_catalog_namespace(&value).expect("生成した namespace を解決できること");
        assert_eq!(
            serialize_catalog_namespace(&namespace).expect("カタログ形式に戻せること"),
            value,
            "parse / serialize が往復すること"
        );
        Ok(())
    })?;
    Ok(())
}

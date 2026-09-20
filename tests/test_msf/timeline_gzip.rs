use super::*;

/// 解凍爆弾 (gzip bomb) を生成する
///
/// 同一バイトの繰り返しを `noflate::gzip::Encoder` で圧縮し、
/// 展開後サイズが 17 MiB (> 16 MiB 上限) となる小さな gzip ペイロードを作る。
/// 実体化する平文は小さなチャンクに抑え、巨大な平文をメモリ上に持たずに bomb を構築する。
fn build_gzip_bomb() -> Vec<u8> {
    // 展開後の目標サイズ: 17 MiB (上限 16 MiB を確実に超える)
    let target_uncompressed: usize = 17 * 1024 * 1024;
    // feed するチャンク (リテラルバイト 'A' の繰り返しでよく圧縮される)
    let chunk: Vec<u8> = vec![b'A'; 64 * 1024];

    let mut enc = noflate::gzip::Encoder::new();
    let mut compressed = Vec::new();
    let mut fed = 0;

    while fed < target_uncompressed {
        let remaining = target_uncompressed - fed;
        let to_feed = remaining.min(chunk.len());
        enc.feed(&chunk[..to_feed])
            .expect("些細なデータでは gzip encoder の feed は成功する");
        // feed のたびに出力をドレインして圧縮列だけを蓄積する
        let produced = enc.output().to_vec();
        let n = produced.len();
        compressed.extend_from_slice(&produced);
        enc.advance(n);
        fed += to_feed;
    }
    // 仕上げで trailer を書き出す
    enc.finish()
        .expect("些細なデータでは gzip encoder の finish は成功する");
    let fin = enc.output().to_vec();
    let n = fin.len();
    compressed.extend_from_slice(&fin);
    enc.advance(n);
    compressed
}

fn sample_media_timeline() -> MsfMediaTimeline {
    let mut tl = MsfMediaTimeline::new();
    for i in 0..4 {
        tl.0.push(MsfMediaTimelineEntry {
            group_id: i,
            object_id: 0,
            wallclock_ms: 1_700_000_000_000 + i * 1000,
            pts_ms: i * 1000,
        });
    }
    tl
}

fn sample_event_timeline() -> MsfEventTimeline {
    let mut tl = MsfEventTimeline::new();
    for i in 0..4 {
        tl.0.push(MsfEventTimelineEntry {
            index: MsfEventIndex::WallclockMs(1000 + i),
            data_raw: br#"{"k":1}"#.to_vec(),
        });
    }
    tl
}

#[test]
fn media_timeline_roundtrip_gzip_false() {
    let tl = sample_media_timeline();
    let encoded = encode_media_timeline(&tl, TimelineEncodingOptions { gzip: false })
        .expect("テストフィクスチャの前提条件を満たす");
    // 生 JSON は "[" で始まる
    assert_eq!(encoded[0], b'[');
    let decoded = decode_media_timeline(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, tl);
}

#[test]
fn encode_event_timeline_gzip_propagates_data_raw_error() {
    // 不正な data_raw は encode 検証エラーになり、gzip 圧縮前に伝播する
    let tl = MsfEventTimeline(vec![MsfEventTimelineEntry {
        index: MsfEventIndex::WallclockMs(1000),
        data_raw: vec![0x80, 0x81],
    }]);
    let err = encode_event_timeline(&tl, TimelineEncodingOptions { gzip: true }).unwrap_err();
    assert!(matches!(err, MessageError::InvalidCatalog(_)));
}

#[test]
fn event_timeline_roundtrip_gzip_false() {
    let tl = sample_event_timeline();
    let encoded = encode_event_timeline(&tl, TimelineEncodingOptions { gzip: false })
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(encoded[0], b'[');
    let decoded = decode_event_timeline(&encoded).expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(decoded, tl);
}

#[test]
fn decode_rejects_corrupted_gzip() {
    // gzip magic を付けた壊れたペイロード
    let mut bad = vec![0x1F, 0x8B, 0x08, 0x00];
    bad.extend_from_slice(&[0xFF; 16]);
    let err = decode_media_timeline(&bad).unwrap_err();
    assert!(matches!(err, MessageError::GzipDecode(_)));
}

#[test]
fn decode_empty_buffer_as_raw_json_fails() {
    // 空バッファは gzip magic 判定で non-gzip となり、JSON パースで失敗する
    let err = decode_media_timeline(&[]).unwrap_err();
    assert!(matches!(err, MessageError::InvalidCatalog(_)));
}

#[test]
fn decode_rejects_truncated_gzip() {
    // gzip magic を付けた生 DEFLATE ストリームを途中で切る
    let mut compressed = vec![0x1F, 0x8B, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03];
    compressed.extend_from_slice(&[0x63, 0x60, 0x60, 0x60, 0x00, 0x00, 0x00]);
    // trailer を切り落とす
    compressed.truncate(compressed.len() - 4);
    let err = decode_media_timeline(&compressed).unwrap_err();
    assert!(matches!(err, MessageError::GzipDecode(_)));
}

#[test]
fn decode_rejects_gzip_bomb() {
    // 展開後 17 MiB の gzip bomb を生成し、上限 16 MiB で拒否されることを確認する
    let bomb = build_gzip_bomb();
    // gzip magic が正しく付いていることを確認
    assert_eq!(&bomb[..2], &[0x1F, 0x8B], "bomb は gzip magic で始まる");
    // 圧縮後のサイズが展開後より十分小さい (高圧縮率) ことを確認
    assert!(
        bomb.len() < 1024 * 1024,
        "bomb 圧縮後サイズ {} バイトは想定より大きい",
        bomb.len()
    );

    let err = decode_media_timeline(&bomb).unwrap_err();
    assert!(
        matches!(err, MessageError::GzipDecode(_)),
        "gzip bomb は GzipDecode エラーになる: got {err:?}"
    );
}

#[test]
fn gzip_is_smaller_for_many_entries() {
    // 1000 件相当の冗長データで gzip の方が小さくなることを確認する
    let mut tl = MsfMediaTimeline::new();
    for i in 0..1000 {
        tl.0.push(MsfMediaTimelineEntry {
            group_id: i,
            object_id: 0,
            wallclock_ms: 1_700_000_000_000,
            pts_ms: 0,
        });
    }
    let raw = encode_media_timeline(&tl, TimelineEncodingOptions { gzip: false })
        .expect("テストフィクスチャの前提条件を満たす");
    let gz = encode_media_timeline(&tl, TimelineEncodingOptions { gzip: true })
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(
        gz.len() < raw.len() / 2,
        "gzip should compress repetitive payload: raw={} gz={}",
        raw.len(),
        gz.len()
    );
}

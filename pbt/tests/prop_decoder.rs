//! `MessageDecoder` のインクリメンタルデコード不変式の PBT
//!
//! draft-ietf-moq-transport-21 §6.3.2 (Session initialization): 制御メッセージは
//! QUIC / WebTransport のフレーム境界と一致しないため、受信バッファへの蓄積と
//! 分割デコードで結果が変わってはならない。

use pbt::common::test_runner;
use shiguredo_moqt::{
    decoder::MessageDecoder,
    message::{ControlMessage, Goaway, Setup, Subscribe, common::TrackNamespace},
    message_parameter::MessageParameters,
    parameter::SetupOptions,
};

/// 代表メッセージ 3 種 (SETUP / GOAWAY / SUBSCRIBE) のいずれかを生成する
fn sample_message(ctx: &mut noprop::TestCaseContext) -> ControlMessage {
    match noprop::sample_usize_in(ctx, 0..3) {
        0 => ControlMessage::Setup(Setup {
            options: SetupOptions::new(),
        }),
        1 => {
            let len = noprop::sample_usize_in(ctx, 0..=16);
            let mut uri = Vec::new();
            for _ in 0..len {
                uri.push(noprop::sample_u8(ctx));
            }
            ControlMessage::Goaway(Goaway {
                new_session_uri: uri,
                timeout: noprop::sample_u64(ctx),
            })
        }
        _ => {
            let ns_len = noprop::sample_usize_in(ctx, 1..=3);
            let mut fields = Vec::new();
            for _ in 0..ns_len {
                let len = noprop::sample_usize_in(ctx, 1..=4);
                let mut field = Vec::new();
                for _ in 0..len {
                    field.push(noprop::sample_u8(ctx));
                }
                fields.push(field);
            }
            ControlMessage::Subscribe(Subscribe {
                request_id: noprop::sample_u64(ctx),
                track_namespace: TrackNamespace::new(fields)
                    .expect("テストフィクスチャの前提条件を満たす"),
                track_name: b"cam".to_vec(),
                parameters: MessageParameters::new(),
            })
        }
    }
}

/// 任意分割で push してもデコード結果が同じである
#[test]
fn decoder_split_invariance() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let msg = sample_message(ctx);
        let bytes = msg.encode().expect("テストフィクスチャの前提条件を満たす");
        // 分割点 (両端を含む) で 2 分割する
        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        let mut whole = MessageDecoder::new();
        whole.push(&bytes);
        let mut split = MessageDecoder::new();
        split.push(&bytes[..at]);
        split.push(&bytes[at..]);
        assert_eq!(
            whole.try_decode_message(),
            split.try_decode_message(),
            "分割デコードの結果が一括と一致すること"
        );
        Ok(())
    })?;
    Ok(())
}

/// 不正バイト列に対して decoder が panic せずエラーを返す
#[test]
fn decoder_garbage_never_panics() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let len = noprop::sample_usize_in(ctx, 0..=32);
        let mut bytes = Vec::new();
        for _ in 0..len {
            bytes.push(noprop::sample_u8(ctx));
        }
        // 一括と分割で結果が一致し、どちらも panic しないことだけ検証する
        // (Ok / Err のいずれも正当。エラー種別の仕様化は単体テストの責務)
        let mut whole = MessageDecoder::new();
        whole.push(&bytes);
        let whole_result = whole.try_decode_message().map(|_| ());
        let mut split = MessageDecoder::new();
        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        split.push(&bytes[..at]);
        split.push(&bytes[at..]);
        let split_result = split.try_decode_message().map(|_| ());
        assert_eq!(
            whole_result.is_ok(),
            split_result.is_ok(),
            "分割しても成否が変わらないこと"
        );
        Ok(())
    })?;
    Ok(())
}

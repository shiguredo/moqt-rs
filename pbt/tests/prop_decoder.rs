//! `MessageDecoder` のインクリメンタルデコード不変式の PBT
//!
//! draft-ietf-moq-transport-23 §6.3 (Session initialization) / §9 (Control Messages):
//! 制御メッセージは QUIC / WebTransport のフレーム境界と一致しないため、受信バッファへの
//! 蓄積と分割デコードで結果が変わってはならない。Length は vi64 であり、非最小符号化も
//! 受理する (§8.1 (Variable-Length Integers))。

use pbt::common::test_runner;
use shiguredo_moqt::{
    decoder::MessageDecoder,
    error::MessageError,
    message::{ControlMessage, Goaway, Setup, Subscribe, common::TrackNamespace},
    message_parameter::MessageParameters,
    parameter::{DEFAULT_MAX_CONTROL_MESSAGE_SIZE, SetupOptions},
    varint,
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

/// 指定したバイト幅の vi64 を非最小符号化で書き出す
///
/// `width` は 1〜9。`value` はその幅で表せる範囲に収まっている必要がある。
/// draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers): decode 側は非最小
/// エンコーディングを受理するため、境界 (1 バイト / 9 バイト) を含む幅で検証する。
fn encode_varint_with_width(value: u64, width: usize) -> Vec<u8> {
    assert!((1..=9).contains(&width), "vi64 の幅は 1〜9 であること");
    if width == 1 {
        assert!(value <= 0x7F, "1 バイト vi64 の値域を超えている");
        return vec![value as u8];
    }
    // 先頭バイトのプレフィックスは (width - 1) 個の 1 と 0 (9 バイトは全て 1)
    let prefix = (((1u16 << (width - 1)) - 1) << (9 - width)) as u8;
    let first_value_bits = if width == 9 { 0 } else { 8 - width };
    let first_mask = if first_value_bits == 0 {
        0
    } else {
        ((1u16 << first_value_bits) - 1) as u8
    };
    let mut buf = Vec::with_capacity(width);
    if width == 9 {
        // 9 バイト vi64 の先頭バイトは 0xFF で値ビットを持たない
        buf.push(prefix);
    } else {
        buf.push(prefix | (((value >> (8 * (width - 1))) as u8) & first_mask));
    }
    for shift in (0..width - 1).rev() {
        buf.push((value >> (8 * shift)) as u8);
    }
    buf
}

/// 任意分割で push してもデコード結果が同じである
#[test]
fn decoder_split_invariance() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let msg = sample_message(ctx);
        let bytes = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("テストフィクスチャの前提条件を満たす");
        // 分割点 (両端を含む) で 2 分割する
        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        let mut whole = MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE);
        whole.push(&bytes);
        let mut split = MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE);
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

/// Length を非最小符号化しても、任意分割で同じメッセージにデコードできる
///
/// 最小符号化は `ControlMessage::encode` が行うため、ここでは同一の本文に対して
/// Length のバイト幅だけを変えたフレームを組み立てる。Type も非最小幅で表す。
#[test]
fn decoder_non_minimal_length_split_invariance() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let msg = sample_message(ctx);
        let minimal = msg
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("テストフィクスチャの前提条件を満たす");
        // 最小符号化から本文を取り出す
        let (type_id, type_len) = varint::decode(&minimal).expect("Type を読める");
        let (body_len, length_len) = varint::decode(&minimal[type_len..]).expect("Length を読める");
        let body = &minimal[type_len + length_len..];
        assert_eq!(body.len() as u64, body_len);

        // Type と Length のバイト幅をそれぞれ最小幅以上 9 以下で選ぶ
        let type_width = noprop::sample_usize_in(ctx, varint::encoded_len(type_id)..=9);
        let length_width = noprop::sample_usize_in(ctx, varint::encoded_len(body_len)..=9);
        let mut bytes = encode_varint_with_width(type_id, type_width);
        bytes.extend_from_slice(&encode_varint_with_width(body_len, length_width));
        bytes.extend_from_slice(body);

        let at = noprop::sample_usize_in(ctx, 0..=bytes.len());
        let mut whole = MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE);
        whole.push(&bytes);
        let mut split = MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE);
        split.push(&bytes[..at]);
        split.push(&bytes[at..]);
        let whole_result = whole
            .try_decode_message()
            .expect("非最小符号化は受理されること");
        let split_result = split
            .try_decode_message()
            .expect("非最小符号化は分割しても受理されること");
        assert_eq!(whole_result, split_result, "分割しても同じ結果になること");
        assert_eq!(whole_result, Some(msg), "元のメッセージに戻ること");
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
        let mut whole = MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE);
        whole.push(&bytes);
        let whole_result = whole.try_decode_message().map(|_| ());
        let mut split = MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE);
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

/// 宣言長が受信上限を超えるフレームは、本文の有無によらず FIELD_LENGTH_EXCEEDED になる
///
/// draft-ietf-moq-transport-23 §9 (Control Messages): 上限を超える Length の宣言は
/// FIELD_LENGTH_EXCEEDED で閉じる (MUST)。本文を待たずに拒否するため、本文を 0 バイトしか
/// push しなくてもエラーになること。宣言長は上限のすぐ上から大きく離れた値まで生成する。
#[test]
fn declared_length_over_limit_is_rejected_without_body() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let limit = DEFAULT_MAX_CONTROL_MESSAGE_SIZE;
        // 上限 + 1 から上限 + 1000 の範囲と、varint の上位バイトを使う大きな値
        let over = if noprop::sample_bool(ctx) {
            limit + noprop::sample_u64_in(ctx, 1..=1000)
        } else {
            limit + noprop::sample_u64_in(ctx, 1024..=(1u64 << 40))
        };
        let mut bytes = Vec::new();
        varint::encode(0x2F00, &mut bytes);
        varint::encode(over, &mut bytes);
        // 本文は 1 バイトも push しない
        let mut decoder = MessageDecoder::new(limit);
        decoder.push(&bytes);
        let result = decoder.try_decode_message();
        assert!(
            matches!(result, Err(MessageError::FieldLengthExceeded(_))),
            "宣言長 {over} (上限 {limit}) は本文なしで拒否されること: {result:?}"
        );
        Ok(())
    })?;
    Ok(())
}

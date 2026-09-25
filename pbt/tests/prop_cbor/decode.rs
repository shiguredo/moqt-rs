//! デコーダの Property-Based Testing
//!
//! 有効なエンコードと、それを切り詰め・改変した入力、および完全な乱数バイト列を
//! 生成し、API 間の結果の整合性を検証する。

use super::helpers;

use std::cell::Cell;

use noprop::TestCaseContext;
use shiguredo_moqt::cbor::decode::{Decoder, decode, decode_all};
use shiguredo_moqt::cbor::error::DecodeErrorKind;
use shiguredo_moqt::cbor::value::Value;

/// このファイルの PBT ケース数
const CASES: usize = 512;

/// 有効なエンコードと、それを切り詰め・改変した入力、乱数バイト列を生成する
fn sample_input(ctx: &mut TestCaseContext) -> Vec<u8> {
    let count = noprop::sample_usize_in(ctx, 0..=3);
    let mut bytes = Vec::new();
    for _ in 0..count {
        helpers::sample_value(ctx, 0).encode_into(&mut bytes);
    }

    match noprop::sample_weighted_index(ctx, &[2, 2, 2, 1]) {
        // そのまま
        0 => bytes,
        // 切り詰める
        1 => {
            if !bytes.is_empty() {
                let keep = noprop::sample_usize_in(ctx, 0..bytes.len());
                bytes.truncate(keep);
            }
            bytes
        }
        // 1 バイトだけ改変する
        2 => {
            if !bytes.is_empty() {
                let index = noprop::sample_usize_in(ctx, 0..bytes.len());
                bytes[index] = noprop::sample_u8(ctx);
            }
            bytes
        }
        // 完全な乱数バイト列
        _ => {
            let len = noprop::sample_usize_in(ctx, 0..=12);
            noprop::sample_bytes_vec(ctx, len)
        }
    }
}

#[test]
fn decoders_agree_on_arbitrary_input() -> noprop::TestResult {
    let seed = noprop::seed_from_env_or_time("CBOR_RS_PBT_SEED")?;
    let empty_inputs = Cell::new(0usize);
    let successful = Cell::new(0usize);
    let multi_item = Cell::new(0usize);
    let failed = Cell::new(0usize);
    let mut runner = noprop::Runner::new(seed);

    runner.run(CASES, |ctx| {
        let bytes = sample_input(ctx);

        // decode_all と Decoder の逐次デコードの結果が一致すること
        let all = decode_all(&bytes);
        let mut decoder = Decoder::new(&bytes);
        let mut incremental = Vec::new();
        let incremental_result = loop {
            if decoder.is_finished() {
                break Ok(incremental);
            }
            match decoder.decode() {
                Ok(value) => incremental.push(value),
                Err(error) => break Err(error),
            }
        };
        match (&all, &incremental_result) {
            (Ok(all), Ok(incremental)) => {
                assert_eq!(
                    all,
                    incremental,
                    "decode_all と Decoder の結果が食い違った: {}",
                    bytes_to_hex(&bytes)
                );
                if all.is_empty() {
                    empty_inputs.set(empty_inputs.get() + 1);
                }
                successful.set(successful.get() + 1);
                if all.len() >= 2 {
                    multi_item.set(multi_item.get() + 1);
                }

                // 成功した入力は再エンコードしても同じ値にデコードできること
                let mut reencoded = Vec::new();
                for value in all {
                    value.encode_into(&mut reencoded);
                }
                let decoded_again = decode_all(&reencoded)
                    .expect("再エンコードした CBOR シーケンスは必ずデコードできる");
                assert_eq!(
                    &decoded_again, all,
                    "再エンコードした CBOR シーケンスの値が変わった"
                );
            }
            (Err(all), Err(incremental)) => {
                assert_eq!(
                    all.kind(),
                    incremental.kind(),
                    "decode_all と Decoder のエラー種別が食い違った: {}",
                    bytes_to_hex(&bytes)
                );
                assert_eq!(
                    all.position(),
                    incremental.position(),
                    "decode_all と Decoder のエラー位置が食い違った: {}",
                    bytes_to_hex(&bytes)
                );
                failed.set(failed.get() + 1);
            }
            _ => {
                panic!("decode_all と Decoder の成否が食い違った: {all:?} / {incremental_result:?}")
            }
        }

        // decode は最初の 1 件だけをデコードすること
        let mut first_decoder = Decoder::new(&bytes);
        match first_decoder.decode() {
            Ok(first) => match decode(&bytes) {
                Ok(value) => assert_eq!(value, first, "decode と Decoder の結果が食い違った"),
                Err(error) => assert_eq!(
                    error.kind(),
                    DecodeErrorKind::TrailingData,
                    "入力が残っているのに decode が TrailingData 以外を返した: {error}"
                ),
            },
            Err(error) => {
                let decode_error = decode(&bytes).expect_err("decode が成功した");
                assert_eq!(
                    decode_error.kind(),
                    error.kind(),
                    "decode と Decoder のエラー種別が食い違った: {}",
                    bytes_to_hex(&bytes)
                );
                assert_eq!(
                    decode_error.position(),
                    error.position(),
                    "decode と Decoder のエラー位置が食い違った: {}",
                    bytes_to_hex(&bytes)
                );
            }
        }
        Ok(())
    })?;

    assert!(
        empty_inputs.get() > 0,
        "空の入力が 1 件も生成されなかった\n{runner}"
    );
    assert!(
        successful.get() > 0,
        "デコードに成功する入力が 1 件も生成されなかった\n{runner}"
    );
    assert!(
        multi_item.get() > 0,
        "複数のデータ項目を持つ入力が 1 件も生成されなかった\n{runner}"
    );
    assert!(
        failed.get() > 0,
        "デコードに失敗する入力が 1 件も生成されなかった\n{runner}"
    );
    Ok(())
}

#[test]
fn sequence_round_trip() -> noprop::TestResult {
    let seed = noprop::seed_from_env_or_time("CBOR_RS_PBT_SEED")?;
    let multi_item = Cell::new(0usize);
    let mut runner = noprop::Runner::new(seed);

    runner.run(CASES, |ctx| {
        let count = noprop::sample_usize_in(ctx, 1..=4);
        let mut values: Vec<Value> = Vec::new();
        let mut bytes = Vec::new();
        for _ in 0..count {
            let value = helpers::sample_value(ctx, 0);
            value.encode_into(&mut bytes);
            values.push(value);
        }

        // decode_all の往復で値が変わらないこと
        let decoded =
            decode_all(&bytes).expect("エンコードした CBOR シーケンスは必ずデコードできる");
        assert_eq!(decoded, values, "CBOR シーケンスの往復で値が変わった");

        // 2 件以上のときは decode が最初の 1 件の直後を TrailingData として報告すること
        if values.len() >= 2 {
            multi_item.set(multi_item.get() + 1);
            let first_len = values[0].to_bytes().len();
            let error = decode(&bytes).expect_err("decode が成功した");
            assert_eq!(error.kind(), DecodeErrorKind::TrailingData);
            assert_eq!(
                error.position(),
                first_len,
                "TrailingData の位置が最初のデータ項目の直後でない"
            );
        }
        Ok(())
    })?;

    assert!(
        multi_item.get() > 0,
        "複数のデータ項目を持つ CBOR シーケンスが 1 件も生成されなかった\n{runner}"
    );
    Ok(())
}

/// バイト列を 16 進数の文字列に変換する (失敗メッセージ用)
fn bytes_to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

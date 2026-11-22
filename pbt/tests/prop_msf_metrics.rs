//! MSF Metrics payload の PBT
//!
//! draft-jennings-moq-metrics-03 §5.3 (Catalog Track) / §5.4 (Groups and Objects):
//! 任意の正当な Catalog Header / Metric Description / Capture Info / Metric Value に
//! ついて、encode → decode が元の値を復元すること、定義済みフィールドを欠く payload が
//! 拒否されること、定義済みフィールドの後ろのバイトが無視されることを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::msf::metrics::{
    CaptureInfo, CatalogHeader, MetricAttribute, MetricDescription, MetricType, MetricValue,
    ValueRepresentation,
};

/// 空を含む UTF-8 文字列を生成する
fn sample_utf8_string(ctx: &mut noprop::TestCaseContext, max_len: usize) -> String {
    let len = noprop::sample_usize_in(ctx, 0..=max_len);
    noprop::sample_string(ctx, len)
}

/// §4 の文字種だけから成る Metric 名を生成する
///
/// draft-jennings-moq-metrics-03 §4 (Metrics Data Model): ASCII の英字・数字・`_`・`:`。
fn sample_metric_name(ctx: &mut noprop::TestCaseContext) -> String {
    let len = noprop::sample_usize_in(ctx, 0..=12);
    (0..len)
        .map(|_| char::from(noprop::sample_choice(ctx, b"abcXYZ019_:")))
        .collect()
}

/// Metric Index (N > 0) を生成する
fn sample_metric_index(ctx: &mut noprop::TestCaseContext) -> u64 {
    noprop::sample_with_boundaries(ctx, &[1, 2, u64::MAX], noprop::Ratio::one_nth(5), |ctx| {
        noprop::sample_u64_in(ctx, 1..=u64::MAX)
    })
}

/// 任意の Value Representation を生成する
fn sample_representation(ctx: &mut noprop::TestCaseContext) -> ValueRepresentation {
    noprop::sample_choice(
        ctx,
        &[
            ValueRepresentation::Unsigned,
            ValueRepresentation::Signed,
            ValueRepresentation::Float,
        ],
    )
}

/// §5.3 の制約を満たす Catalog Header を生成する
fn sample_catalog_header(ctx: &mut noprop::TestCaseContext) -> CatalogHeader {
    let attribute_count = noprop::sample_usize_in(ctx, 0..=3);
    let attributes = (0..attribute_count)
        .map(|_| MetricAttribute {
            key: sample_utf8_string(ctx, 12),
            value: sample_utf8_string(ctx, 24),
        })
        .collect();
    CatalogHeader {
        version: noprop::sample_with_boundaries(
            ctx,
            &[0, 1, u64::MAX],
            noprop::Ratio::one_nth(5),
            noprop::sample_u64,
        ),
        attributes,
    }
}

/// §5.3 の制約を満たす Metric Description を生成する
fn sample_metric_description(ctx: &mut noprop::TestCaseContext) -> MetricDescription {
    MetricDescription {
        metric_index: sample_metric_index(ctx),
        metric_type: noprop::sample_choice(ctx, &[MetricType::Gauge, MetricType::Counter]),
        value_representation: sample_representation(ctx),
        name: sample_metric_name(ctx),
        unit: sample_utf8_string(ctx, 12),
    }
}

/// §5.4 の制約を満たす Capture Info を生成する
fn sample_capture_info(ctx: &mut noprop::TestCaseContext) -> CaptureInfo {
    CaptureInfo {
        capture_timestamp_offset: noprop::sample_with_boundaries(
            ctx,
            &[0, 999_999],
            noprop::Ratio::one_nth(5),
            |ctx| noprop::sample_u64_in(ctx, 0..=999_999),
        ),
        catalog_version: noprop::sample_with_boundaries(
            ctx,
            &[0, 1, u64::MAX],
            noprop::Ratio::one_nth(5),
            noprop::sample_u64,
        ),
    }
}

/// §5.4 の制約を満たす Metric Value を生成する
fn sample_metric_value(ctx: &mut noprop::TestCaseContext) -> MetricValue {
    let metric_index = sample_metric_index(ctx);
    match sample_representation(ctx) {
        ValueRepresentation::Unsigned => MetricValue::Unsigned {
            metric_index,
            value: noprop::sample_u64(ctx),
        },
        ValueRepresentation::Signed => MetricValue::Signed {
            metric_index,
            value: noprop::sample_i64(ctx),
        },
        ValueRepresentation::Float => MetricValue::Float {
            metric_index,
            value: noprop::sample_f64(ctx),
        },
    }
}

/// 任意の Catalog Header が encode → decode で復元されること
#[test]
fn catalog_header_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let header = sample_catalog_header(ctx);
        let encoded = header.encode();
        let decoded = CatalogHeader::decode(&encoded).expect("エンコード結果をデコードできること");
        assert_eq!(decoded, header, "Catalog Header の往復で値が変わらないこと");
        Ok(())
    })?;
    Ok(())
}

/// Catalog Header の定義済みフィールドの後ろの任意バイトが無視されること
#[test]
fn catalog_header_ignores_trailing_bytes() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let header = sample_catalog_header(ctx);
        let mut encoded = header.encode();
        let extra_len = noprop::sample_usize_in(ctx, 0..=8);
        encoded.extend_from_slice(&noprop::sample_bytes_vec(ctx, extra_len));
        let decoded = CatalogHeader::decode(&encoded)
            .expect("定義済みフィールドの後ろのバイトは無視されること");
        assert_eq!(decoded, header, "後続バイトを無視しても値が変わらないこと");
        Ok(())
    })?;
    Ok(())
}

/// 任意の Metric Description が encode → decode で復元されること
#[test]
fn metric_description_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let description = sample_metric_description(ctx);
        let encoded = description
            .encode()
            .expect("正当な Metric Description はエンコードできること");
        let decoded = MetricDescription::decode(&encoded, description.metric_index)
            .expect("エンコード結果をデコードできること");
        assert_eq!(
            decoded, description,
            "Metric Description の往復で値が変わらないこと"
        );
        Ok(())
    })?;
    Ok(())
}

/// Metric Description の定義済みフィールドの後ろの任意バイトが無視されること
#[test]
fn metric_description_ignores_trailing_bytes() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let description = sample_metric_description(ctx);
        let mut encoded = description
            .encode()
            .expect("正当な Metric Description はエンコードできること");
        let extra_len = noprop::sample_usize_in(ctx, 0..=8);
        encoded.extend_from_slice(&noprop::sample_bytes_vec(ctx, extra_len));
        let decoded = MetricDescription::decode(&encoded, description.metric_index)
            .expect("定義済みフィールドの後ろのバイトは無視されること");
        assert_eq!(
            decoded, description,
            "後続バイトを無視しても値が変わらないこと"
        );
        Ok(())
    })?;
    Ok(())
}

/// 任意の Capture Info が encode → decode で復元されること
#[test]
fn capture_info_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let info = sample_capture_info(ctx);
        let encoded = info
            .encode()
            .expect("正当な Capture Info はエンコードできること");
        let decoded = CaptureInfo::decode(&encoded).expect("エンコード結果をデコードできること");
        assert_eq!(decoded, info, "Capture Info の往復で値が変わらないこと");
        Ok(())
    })?;
    Ok(())
}

/// Capture Info の定義済みフィールドの後ろの任意バイトが無視されること
#[test]
fn capture_info_ignores_trailing_bytes() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let info = sample_capture_info(ctx);
        let mut encoded = info
            .encode()
            .expect("正当な Capture Info はエンコードできること");
        let extra_len = noprop::sample_usize_in(ctx, 0..=8);
        encoded.extend_from_slice(&noprop::sample_bytes_vec(ctx, extra_len));
        let decoded = CaptureInfo::decode(&encoded)
            .expect("定義済みフィールドの後ろのバイトは無視されること");
        assert_eq!(decoded, info, "後続バイトを無視しても値が変わらないこと");
        Ok(())
    })?;
    Ok(())
}

/// 任意の Metric Value が encode → decode で復元されること
#[test]
fn metric_value_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let value = sample_metric_value(ctx);
        let representation = value.representation();
        let metric_index = value.metric_index();
        let encoded = value
            .encode()
            .expect("正当な Metric Value はエンコードできること");
        let decoded = MetricValue::decode(&encoded, metric_index, representation)
            .expect("エンコード結果をデコードできること");
        match (&decoded, &value) {
            (
                MetricValue::Float {
                    metric_index: decoded_index,
                    value: decoded_value,
                },
                MetricValue::Float {
                    metric_index: value_index,
                    value: expected_value,
                },
            ) => {
                assert_eq!(decoded_index, value_index, "Metric Index が保たれること");
                assert_eq!(
                    decoded_value.to_bits(),
                    expected_value.to_bits(),
                    "float のビット列が保たれること"
                );
            }
            (decoded, value) => {
                assert_eq!(decoded, value, "Metric Value の往復で値が変わらないこと");
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Metric Value の定義済みフィールドの後ろの任意バイトが無視されること
#[test]
fn metric_value_ignores_trailing_bytes() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let value = sample_metric_value(ctx);
        let representation = value.representation();
        let metric_index = value.metric_index();
        let mut encoded = value
            .encode()
            .expect("正当な Metric Value はエンコードできること");
        let extra_len = noprop::sample_usize_in(ctx, 0..=8);
        encoded.extend_from_slice(&noprop::sample_bytes_vec(ctx, extra_len));
        let decoded = MetricValue::decode(&encoded, metric_index, representation)
            .expect("定義済みフィールドの後ろのバイトは無視されること");
        match (&decoded, &value) {
            (
                MetricValue::Float {
                    value: decoded_value,
                    ..
                },
                MetricValue::Float {
                    value: expected_value,
                    ..
                },
            ) => {
                assert_eq!(
                    decoded_value.to_bits(),
                    expected_value.to_bits(),
                    "後続バイトを無視しても float のビット列が変わらないこと"
                );
            }
            (decoded, value) => {
                assert_eq!(decoded, value, "後続バイトを無視しても値が変わらないこと");
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// Catalog Header の定義済みフィールドを欠く payload が拒否されること
#[test]
fn truncated_catalog_header_rejected() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let encoded = sample_catalog_header(ctx).encode();
        for cut in 0..encoded.len() {
            assert!(
                CatalogHeader::decode(&encoded[..cut]).is_err(),
                "先頭 {cut} バイトだけの Catalog Header は拒否されること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// Metric Description の定義済みフィールドを欠く payload が拒否されること
#[test]
fn truncated_metric_description_rejected() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let description = sample_metric_description(ctx);
        let encoded = description
            .encode()
            .expect("正当な Metric Description はエンコードできること");
        for cut in 0..encoded.len() {
            assert!(
                MetricDescription::decode(&encoded[..cut], description.metric_index).is_err(),
                "先頭 {cut} バイトだけの Metric Description は拒否されること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// Capture Info の定義済みフィールドを欠く payload が拒否されること
#[test]
fn truncated_capture_info_rejected() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let encoded = sample_capture_info(ctx)
            .encode()
            .expect("正当な Capture Info はエンコードできること");
        for cut in 0..encoded.len() {
            assert!(
                CaptureInfo::decode(&encoded[..cut]).is_err(),
                "先頭 {cut} バイトだけの Capture Info は拒否されること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// Metric Value の定義済みフィールドを欠く payload が拒否されること
#[test]
fn truncated_metric_value_rejected() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let value = sample_metric_value(ctx);
        let representation = value.representation();
        let metric_index = value.metric_index();
        let encoded = value
            .encode()
            .expect("正当な Metric Value はエンコードできること");
        for cut in 0..encoded.len() {
            assert!(
                MetricValue::decode(&encoded[..cut], metric_index, representation).is_err(),
                "先頭 {cut} バイトだけの Metric Value は拒否されること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

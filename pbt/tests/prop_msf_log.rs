//! MSF Log Record の PBT
//!
//! draft-jennings-moq-log-04 §5.3 (Log Record): 任意の正当な Log Record について、
//! encode → decode が元の値を復元すること、定義済みフィールドを欠く payload が拒否される
//! こと、定義済みフィールドの後ろのバイトが無視されることを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::msf::log::{LogRecord, LogRecordAttribute, LogSeverity};

/// 空を含む ASCII 文字列を生成する
fn sample_ascii_string(ctx: &mut noprop::TestCaseContext, max_len: usize) -> String {
    let len = noprop::sample_usize_in(ctx, 0..=max_len);
    noprop::sample_ascii_printable_string(ctx, len)
}

/// 空を含む UTF-8 文字列を生成する
fn sample_utf8_string(ctx: &mut noprop::TestCaseContext, max_len: usize) -> String {
    let len = noprop::sample_usize_in(ctx, 0..=max_len);
    noprop::sample_string(ctx, len)
}

/// §5.3 の制約を満たす Log Record を生成する
///
/// Timestamp Offset は 1,000,000,000 未満、Facility は 0〜23、Hostname / App Name /
/// Proc ID / Msg ID は ASCII、Message と Attribute は UTF-8 とする。
fn sample_log_record(ctx: &mut noprop::TestCaseContext) -> LogRecord {
    let attribute_count = noprop::sample_usize_in(ctx, 0..=4);
    let attributes = (0..attribute_count)
        .map(|_| LogRecordAttribute {
            key: sample_utf8_string(ctx, 12),
            value: sample_utf8_string(ctx, 24),
        })
        .collect();
    LogRecord {
        timestamp_offset: noprop::sample_with_boundaries(
            ctx,
            &[0, 999_999_999],
            noprop::Ratio::one_nth(5),
            |ctx| noprop::sample_u64_in(ctx, 0..=999_999_999),
        ),
        facility: noprop::sample_with_boundaries(ctx, &[0, 23], noprop::Ratio::one_nth(5), |ctx| {
            noprop::sample_u64_in(ctx, 0..=23)
        }),
        hostname: sample_ascii_string(ctx, 16),
        app_name: sample_ascii_string(ctx, 16),
        proc_id: sample_ascii_string(ctx, 16),
        msg_id: sample_ascii_string(ctx, 16),
        message: sample_utf8_string(ctx, 32),
        attributes,
    }
}

/// 任意の正当な Log Record が encode → decode で復元されること
#[test]
fn log_record_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let record = sample_log_record(ctx);
        let encoded = record
            .encode()
            .expect("正当な Log Record はエンコードできること");
        let decoded = LogRecord::decode(&encoded).expect("エンコード結果をデコードできること");
        assert_eq!(decoded, record, "Log Record の往復で値が変わらないこと");
        Ok(())
    })?;
    Ok(())
}

/// 定義済みフィールドの後ろの任意バイトが無視されること
#[test]
fn log_record_ignores_trailing_bytes() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let record = sample_log_record(ctx);
        let mut encoded = record
            .encode()
            .expect("正当な Log Record はエンコードできること");
        let extra_len = noprop::sample_usize_in(ctx, 0..=8);
        encoded.extend_from_slice(&noprop::sample_bytes_vec(ctx, extra_len));
        let decoded =
            LogRecord::decode(&encoded).expect("定義済みフィールドの後ろのバイトは無視されること");
        assert_eq!(
            decoded, record,
            "後続バイトを無視しても Log Record の値が変わらないこと"
        );
        Ok(())
    })?;
    Ok(())
}

/// 定義済みフィールドを欠く payload が拒否されること
#[test]
fn truncated_log_record_rejected() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let record = sample_log_record(ctx);
        let encoded = record
            .encode()
            .expect("正当な Log Record はエンコードできること");
        for cut in 0..encoded.len() {
            assert!(
                LogRecord::decode(&encoded[..cut]).is_err(),
                "先頭 {cut} バイトだけの payload は拒否されること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// §5.1 の severity と Track Name の往復
#[test]
fn severity_track_name_roundtrip() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(64, |ctx| {
        let severity = noprop::sample_choice(
            ctx,
            &[
                LogSeverity::Emergency,
                LogSeverity::Alert,
                LogSeverity::Critical,
                LogSeverity::Error,
                LogSeverity::Warning,
                LogSeverity::Notice,
                LogSeverity::Informational,
                LogSeverity::Debug,
            ],
        );
        let name = severity.track_name();
        assert_eq!(
            LogSeverity::from_track_name(name.as_bytes())
                .expect("既知の Track Name は解釈できること"),
            severity,
            "severity と Track Name の往復"
        );
        Ok(())
    })?;
    Ok(())
}

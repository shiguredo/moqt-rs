//! `src/error.rs` のエラーコード正規化のプロパティ
//!
//! - draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) / §16.12.1
//!   (Session Termination Error Codes)
//! - draft-ietf-moq-transport-23 §12.3 (Request Error Codes) / §16.12.2 (REQUEST_ERROR Codes)
//! - draft-ietf-moq-transport-23 §12.4 (Publish Done Codes) / §16.12.3 (PUBLISH_DONE Codes)
//! - draft-ietf-moq-transport-23 §12.5 (Stream Reset Error Codes) / §16.12.4
//!   (Stream Reset Error Codes)
//! - draft-ietf-moq-transport-23 §13 (Grease)
//!
//! - 定義済みのコードは正規化で `INTERNAL_ERROR` に落ちない
//! - 未知のコードは `INTERNAL_ERROR` 相当に正規化される
//!
//! 定義済みコードの集合を広げたときに、正規化の写像から漏れたコードが黙って
//! `INTERNAL_ERROR` に落ちる regressions を検出する。

use pbt::common::test_runner;
use shiguredo_moqt::error::{
    PUBLISH_DONE_EXCESSIVE_LOAD, PUBLISH_DONE_EXPIRED, PUBLISH_DONE_EXPIRED_AUTH_TOKEN,
    PUBLISH_DONE_GOING_AWAY, PUBLISH_DONE_INTERNAL_ERROR, PUBLISH_DONE_MALFORMED_TRACK,
    PUBLISH_DONE_TOO_FAR_BEHIND, PUBLISH_DONE_TRACK_ENDED, PUBLISH_DONE_UNAUTHORIZED,
    PUBLISH_DONE_UPDATE_FAILED, REQUEST_CONFLICTING_FILTERS, REQUEST_DOES_NOT_EXIST,
    REQUEST_EXCESSIVE_LOAD, REQUEST_EXPIRED_AUTH_TOKEN, REQUEST_GOING_AWAY, REQUEST_INTERNAL_ERROR,
    REQUEST_INVALID_FILTER, REQUEST_INVALID_RANGE, REQUEST_MALFORMED_AUTH_TOKEN,
    REQUEST_MALFORMED_TRACK, REQUEST_NAMESPACE_TOO_LARGE, REQUEST_NOT_SUPPORTED, REQUEST_REDIRECT,
    REQUEST_TIMEOUT, REQUEST_UNAUTHORIZED, REQUEST_UNAVAILABLE, REQUEST_UNINTERESTED,
    REQUEST_UNSUPPORTED_EXTENSION, SESSION_AUTH_TOKEN_CACHE_OVERFLOW,
    SESSION_CLOSED_CONTROL_STREAM, SESSION_CONTROL_MESSAGE_TIMEOUT, SESSION_DATA_STREAM_TIMEOUT,
    SESSION_DELTA_ENCODING_OVERFLOW, SESSION_DUPLICATE_AUTH_TOKEN_ALIAS,
    SESSION_DUPLICATE_TRACK_ALIAS, SESSION_EXPIRED_AUTH_TOKEN, SESSION_FIELD_LENGTH_EXCEEDED,
    SESSION_GOAWAY_TIMEOUT, SESSION_INTERNAL_ERROR, SESSION_INVALID_AUTHORITY,
    SESSION_INVALID_FIELD, SESSION_INVALID_OBJECT_STATUS, SESSION_INVALID_PARAMETER,
    SESSION_INVALID_PATH, SESSION_INVALID_REQUEST_ID, SESSION_INVALID_STREAM_HEADER,
    SESSION_INVALID_TRACK_NAMESPACE, SESSION_MALFORMED_AUTH_TOKEN, SESSION_MALFORMED_AUTHORITY,
    SESSION_MALFORMED_KEY_VALUE, SESSION_MALFORMED_MESSAGE, SESSION_MALFORMED_OBJECT,
    SESSION_MALFORMED_PATH, SESSION_NO_ERROR, SESSION_PROTOCOL_VIOLATION, SESSION_ROLE_VIOLATION,
    SESSION_TOO_MANY_REQUEST_UPDATES, SESSION_TRUNCATED_OBJECT, SESSION_UNAUTHORIZED,
    SESSION_UNEXPECTED_MESSAGE, SESSION_UNKNOWN_AUTH_TOKEN_ALIAS, STREAM_CANCELLED,
    STREAM_DELIVERY_TIMEOUT, STREAM_EXCESSIVE_LOAD, STREAM_EXPIRED_AUTH_TOKEN, STREAM_GOING_AWAY,
    STREAM_INTERNAL_ERROR, STREAM_MALFORMED_TRACK, STREAM_SESSION_CLOSED, STREAM_TOO_FAR_BEHIND,
    normalize_publish_done_code, normalize_request_error_code, normalize_session_termination_code,
    normalize_stream_reset_code,
};

/// draft-ietf-moq-transport-23 §12.2 / §16.12.1 で定義された全セッション終了コード
///
/// Table 20 に載るコードと、コード表で定義された 0x6 (MALFORMED_KEY_VALUE) を並べる。
const DEFINED_SESSION_CODES: &[u64] = &[
    SESSION_NO_ERROR,
    SESSION_INTERNAL_ERROR,
    SESSION_UNAUTHORIZED,
    SESSION_PROTOCOL_VIOLATION,
    SESSION_INVALID_REQUEST_ID,
    SESSION_DUPLICATE_TRACK_ALIAS,
    SESSION_MALFORMED_KEY_VALUE,
    SESSION_INVALID_PATH,
    SESSION_MALFORMED_PATH,
    SESSION_GOAWAY_TIMEOUT,
    SESSION_CONTROL_MESSAGE_TIMEOUT,
    SESSION_DATA_STREAM_TIMEOUT,
    SESSION_AUTH_TOKEN_CACHE_OVERFLOW,
    SESSION_DUPLICATE_AUTH_TOKEN_ALIAS,
    SESSION_MALFORMED_AUTH_TOKEN,
    SESSION_UNKNOWN_AUTH_TOKEN_ALIAS,
    SESSION_EXPIRED_AUTH_TOKEN,
    SESSION_INVALID_AUTHORITY,
    SESSION_MALFORMED_AUTHORITY,
    SESSION_TOO_MANY_REQUEST_UPDATES,
    SESSION_INVALID_STREAM_HEADER,
    SESSION_MALFORMED_OBJECT,
    SESSION_INVALID_OBJECT_STATUS,
    SESSION_TRUNCATED_OBJECT,
    SESSION_MALFORMED_MESSAGE,
    SESSION_INVALID_FIELD,
    SESSION_INVALID_TRACK_NAMESPACE,
    SESSION_UNEXPECTED_MESSAGE,
    SESSION_ROLE_VIOLATION,
    SESSION_INVALID_PARAMETER,
    SESSION_CLOSED_CONTROL_STREAM,
    SESSION_DELTA_ENCODING_OVERFLOW,
    SESSION_FIELD_LENGTH_EXCEEDED,
];

/// draft-ietf-moq-transport-23 §12.3 / §16.12.2 (Table 21) で定義された全 REQUEST_ERROR コード
///
/// 0x30 (PREFIX_OVERLAP) は draft-23 のレジストリから削除されたため含めない。
const DEFINED_REQUEST_ERROR_CODES: &[u64] = &[
    REQUEST_INTERNAL_ERROR,
    REQUEST_UNAUTHORIZED,
    REQUEST_TIMEOUT,
    REQUEST_NOT_SUPPORTED,
    REQUEST_MALFORMED_AUTH_TOKEN,
    REQUEST_EXPIRED_AUTH_TOKEN,
    REQUEST_GOING_AWAY,
    REQUEST_EXCESSIVE_LOAD,
    REQUEST_UNAVAILABLE,
    REQUEST_DOES_NOT_EXIST,
    REQUEST_INVALID_RANGE,
    REQUEST_MALFORMED_TRACK,
    REQUEST_UNINTERESTED,
    REQUEST_NAMESPACE_TOO_LARGE,
    REQUEST_UNSUPPORTED_EXTENSION,
    REQUEST_REDIRECT,
    REQUEST_CONFLICTING_FILTERS,
    REQUEST_INVALID_FILTER,
];

/// draft-ietf-moq-transport-23 §12.4 / §16.12.3 (Table 22) で定義された全 PUBLISH_DONE コード
const DEFINED_PUBLISH_DONE_CODES: &[u64] = &[
    PUBLISH_DONE_INTERNAL_ERROR,
    PUBLISH_DONE_UNAUTHORIZED,
    PUBLISH_DONE_TRACK_ENDED,
    PUBLISH_DONE_GOING_AWAY,
    PUBLISH_DONE_TOO_FAR_BEHIND,
    PUBLISH_DONE_EXPIRED,
    PUBLISH_DONE_EXPIRED_AUTH_TOKEN,
    PUBLISH_DONE_UPDATE_FAILED,
    PUBLISH_DONE_EXCESSIVE_LOAD,
    PUBLISH_DONE_MALFORMED_TRACK,
];

/// draft-ietf-moq-transport-23 §12.5 / §16.12.4 (Table 23) で定義された全 Stream Reset コード
///
/// 0x6 (UNKNOWN_OBJECT_STATUS) は draft-23 のレジストリから削除されたため含めない。
const DEFINED_STREAM_RESET_CODES: &[u64] = &[
    STREAM_INTERNAL_ERROR,
    STREAM_CANCELLED,
    STREAM_DELIVERY_TIMEOUT,
    STREAM_SESSION_CLOSED,
    STREAM_GOING_AWAY,
    STREAM_TOO_FAR_BEHIND,
    STREAM_EXPIRED_AUTH_TOKEN,
    STREAM_EXCESSIVE_LOAD,
    STREAM_MALFORMED_TRACK,
];

/// セッション終了以外の文脈を 1 つ選び、その定義済みコード・`INTERNAL_ERROR`・正規化関数を返す
///
/// `fn` ポインタで正規化関数を選ぶため、各文脈で同じ性質を 1 つの property で検証できる。
fn sample_context(ctx: &mut noprop::TestCaseContext) -> (&'static [u64], u64, fn(u64) -> u64) {
    match noprop::sample_usize_in(ctx, 0..3) {
        0 => (
            DEFINED_REQUEST_ERROR_CODES,
            REQUEST_INTERNAL_ERROR,
            normalize_request_error_code,
        ),
        1 => (
            DEFINED_PUBLISH_DONE_CODES,
            PUBLISH_DONE_INTERNAL_ERROR,
            normalize_publish_done_code,
        ),
        _ => (
            DEFINED_STREAM_RESET_CODES,
            STREAM_INTERNAL_ERROR,
            normalize_stream_reset_code,
        ),
    }
}

/// セッション終了以外の文脈でも、定義済みコードは正規化で保たれること
///
/// REQUEST_ERROR / PUBLISH_DONE / Stream Reset のいずれも、定義済みコードが
/// 文脈ごとの `INTERNAL_ERROR` に落ちると受信側のアプリが原因を失う。
#[test]
fn defined_codes_are_kept_in_each_context() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (defined, _, normalize) = sample_context(ctx);
        let index = noprop::sample_usize_in(ctx, 0..defined.len());
        let code = defined[index];
        assert_eq!(
            normalize(code),
            code,
            "定義済みコード {code:#x} は正規化で保たれること"
        );
        Ok(())
    })?;
    Ok(())
}

/// セッション終了以外の文脈でも、正規化結果は元の値か文脈ごとの `INTERNAL_ERROR` のいずれかであること
///
/// §13 (Grease) の "Receipt of an unknown error code ... MUST be treated as equivalent to
/// INTERNAL_ERROR" に従い、未知コードはその文脈の `INTERNAL_ERROR` に写す。第三の値に
/// 化けないこと (誤ったコードへの置換が無いこと) を検証する。
#[test]
fn normalization_result_is_identity_or_context_internal_error() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let (defined, internal_error, normalize) = sample_context(ctx);
        let code = noprop::sample_u64(ctx);
        let normalized = normalize(code);
        if defined.contains(&code) {
            assert_eq!(normalized, code, "定義済みコード {code:#x} は保たれること");
        } else {
            assert_eq!(
                normalized, internal_error,
                "未知コード {code:#x} は文脈の INTERNAL_ERROR に正規化されること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 定義済みコードは正規化しても自分自身のままであること
///
/// 定義済みコードが `INTERNAL_ERROR` に落ちると、受信側のアプリが peer の終了理由を
/// 分類できなくなる。
#[test]
fn defined_session_codes_are_never_normalized_to_internal_error() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let index = noprop::sample_usize_in(ctx, 0..DEFINED_SESSION_CODES.len());
        let code = DEFINED_SESSION_CODES[index];
        assert_eq!(
            normalize_session_termination_code(code),
            code,
            "定義済みコード {code:#x} は正規化で保たれること"
        );
        Ok(())
    })?;
    Ok(())
}

/// 任意の u64 について、正規化結果は元の値か `INTERNAL_ERROR` のいずれかであること
///
/// §13 (Grease) の "Receipt of an unknown error code ... MUST be treated as equivalent to
/// INTERNAL_ERROR" に従い、未知コードは `INTERNAL_ERROR` に写す。第三の値に化けないこと
/// (誤ったコードへの置換が無いこと) を検証する。
#[test]
fn normalization_result_is_identity_or_internal_error() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let code = noprop::sample_u64(ctx);
        let normalized = normalize_session_termination_code(code);
        if DEFINED_SESSION_CODES.contains(&code) {
            assert_eq!(normalized, code, "定義済みコード {code:#x} は保たれること");
        } else {
            assert_eq!(
                normalized, SESSION_INTERNAL_ERROR,
                "未知コード {code:#x} は INTERNAL_ERROR に正規化されること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 定義済みコードの隣接値 (未割り当て) は `INTERNAL_ERROR` に正規化されること
///
/// 定義済みコードを 1 つ選び、その前後の値のうち定義済みでないものを対象にする。
/// レジストリの隙間 (0x7 / 0x1C / 0x204 など) の扱いが変わらないことを検証する。
#[test]
fn unassigned_neighbors_are_normalized_to_internal_error() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let index = noprop::sample_usize_in(ctx, 0..DEFINED_SESSION_CODES.len());
        let base = DEFINED_SESSION_CODES[index];
        let candidate = if noprop::sample_bool(ctx) {
            base.saturating_add(1)
        } else {
            base.saturating_sub(1)
        };
        if DEFINED_SESSION_CODES.contains(&candidate) {
            return Ok(());
        }
        assert_eq!(
            normalize_session_termination_code(candidate),
            SESSION_INTERNAL_ERROR,
            "未割り当ての隣接値 {candidate:#x} は INTERNAL_ERROR に正規化されること"
        );
        Ok(())
    })?;
    Ok(())
}

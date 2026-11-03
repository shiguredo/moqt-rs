//! `src/error.rs` のセッション終了コード正規化のプロパティ
//!
//! draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) / §16.12.1
//! (Session Termination Error Codes) / §13 (Grease) を根拠とする。
//!
//! - 定義済みのコードは正規化で `INTERNAL_ERROR` に落ちない
//! - 未知のコードは `INTERNAL_ERROR` 相当に正規化される
//!
//! 定義済みコードの集合を広げたときに、正規化の写像から漏れたコードが黙って
//! `INTERNAL_ERROR` に落ちる regressions を検出する。

use pbt::common::test_runner;
use shiguredo_moqt::error::{
    SESSION_AUTH_TOKEN_CACHE_OVERFLOW, SESSION_CLOSED_CONTROL_STREAM,
    SESSION_CONTROL_MESSAGE_TIMEOUT, SESSION_DATA_STREAM_TIMEOUT, SESSION_DELTA_ENCODING_OVERFLOW,
    SESSION_DUPLICATE_AUTH_TOKEN_ALIAS, SESSION_DUPLICATE_TRACK_ALIAS, SESSION_EXPIRED_AUTH_TOKEN,
    SESSION_FIELD_LENGTH_EXCEEDED, SESSION_GOAWAY_TIMEOUT, SESSION_INTERNAL_ERROR,
    SESSION_INVALID_AUTHORITY, SESSION_INVALID_FIELD, SESSION_INVALID_OBJECT_STATUS,
    SESSION_INVALID_PARAMETER, SESSION_INVALID_PATH, SESSION_INVALID_REQUEST_ID,
    SESSION_INVALID_STREAM_HEADER, SESSION_INVALID_TRACK_NAMESPACE, SESSION_MALFORMED_AUTH_TOKEN,
    SESSION_MALFORMED_AUTHORITY, SESSION_MALFORMED_KEY_VALUE, SESSION_MALFORMED_MESSAGE,
    SESSION_MALFORMED_OBJECT, SESSION_MALFORMED_PATH, SESSION_NO_ERROR, SESSION_PROTOCOL_VIOLATION,
    SESSION_ROLE_VIOLATION, SESSION_TOO_MANY_REQUEST_UPDATES, SESSION_TRUNCATED_OBJECT,
    SESSION_UNAUTHORIZED, SESSION_UNEXPECTED_MESSAGE, SESSION_UNKNOWN_AUTH_TOKEN_ALIAS,
    normalize_session_termination_code,
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

//! エラーコード定数が draft-ietf-moq-transport-23 §16.12 / §12.2 (Session Termination Codes)
//! のレジストリ表と一致することと、`MessageError` が `core::error::Error` を実装していることのテスト

use shiguredo_moqt::error::{
    PUBLISH_DONE_EXCESSIVE_LOAD, PUBLISH_DONE_EXPIRED, PUBLISH_DONE_GOING_AWAY,
    PUBLISH_DONE_INTERNAL_ERROR, PUBLISH_DONE_MALFORMED_TRACK, PUBLISH_DONE_TOO_FAR_BEHIND,
    PUBLISH_DONE_TRACK_ENDED, PUBLISH_DONE_UNAUTHORIZED, PUBLISH_DONE_UPDATE_FAILED,
    REQUEST_CONFLICTING_FILTERS, REQUEST_DOES_NOT_EXIST, REQUEST_EXCESSIVE_LOAD,
    REQUEST_EXPIRED_AUTH_TOKEN, REQUEST_GOING_AWAY, REQUEST_INTERNAL_ERROR, REQUEST_INVALID_FILTER,
    REQUEST_INVALID_RANGE, REQUEST_MALFORMED_AUTH_TOKEN, REQUEST_MALFORMED_TRACK,
    REQUEST_NAMESPACE_TOO_LARGE, REQUEST_NOT_SUPPORTED, REQUEST_PREFIX_OVERLAP, REQUEST_REDIRECT,
    REQUEST_TIMEOUT, REQUEST_UNAUTHORIZED, REQUEST_UNINTERESTED, REQUEST_UNSUPPORTED_EXTENSION,
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
    STREAM_CANCELLED, STREAM_DELIVERY_TIMEOUT, STREAM_EXCESSIVE_LOAD, STREAM_EXPIRED_AUTH_TOKEN,
    STREAM_GOING_AWAY, STREAM_INTERNAL_ERROR, STREAM_MALFORMED_TRACK, STREAM_SESSION_CLOSED,
    STREAM_TOO_FAR_BEHIND, STREAM_UNKNOWN_OBJECT_STATUS,
};
use shiguredo_moqt::message::ControlMessage;

/// §16.12.1 (Session Termination Error Codes) の Table 20 と一致する
#[test]
fn session_termination_codes_match_iana_registry() {
    assert_eq!(SESSION_NO_ERROR, 0x0);
    assert_eq!(SESSION_INTERNAL_ERROR, 0x1);
    assert_eq!(SESSION_UNAUTHORIZED, 0x2);
    assert_eq!(SESSION_PROTOCOL_VIOLATION, 0x3);
    assert_eq!(SESSION_INVALID_REQUEST_ID, 0x4);
    assert_eq!(SESSION_DUPLICATE_TRACK_ALIAS, 0x5);
    assert_eq!(SESSION_MALFORMED_KEY_VALUE, 0x6);
    assert_eq!(SESSION_INVALID_PATH, 0x8);
    assert_eq!(SESSION_MALFORMED_PATH, 0x9);
    assert_eq!(SESSION_GOAWAY_TIMEOUT, 0x10);
    assert_eq!(SESSION_CONTROL_MESSAGE_TIMEOUT, 0x11);
    assert_eq!(SESSION_DATA_STREAM_TIMEOUT, 0x12);
    assert_eq!(SESSION_AUTH_TOKEN_CACHE_OVERFLOW, 0x13);
    assert_eq!(SESSION_DUPLICATE_AUTH_TOKEN_ALIAS, 0x14);
    assert_eq!(SESSION_MALFORMED_AUTH_TOKEN, 0x16);
    assert_eq!(SESSION_UNKNOWN_AUTH_TOKEN_ALIAS, 0x17);
    assert_eq!(SESSION_EXPIRED_AUTH_TOKEN, 0x18);
    assert_eq!(SESSION_INVALID_AUTHORITY, 0x19);
    assert_eq!(SESSION_MALFORMED_AUTHORITY, 0x1A);
    assert_eq!(SESSION_TOO_MANY_REQUEST_UPDATES, 0x1B);
    // draft-ietf-moq-transport-23 §12.2 (Session Termination Codes) で追加されたコード
    // (0x200-0x2FF はデータストリーム / データグラム)
    assert_eq!(SESSION_INVALID_STREAM_HEADER, 0x200);
    assert_eq!(SESSION_MALFORMED_OBJECT, 0x201);
    assert_eq!(SESSION_INVALID_OBJECT_STATUS, 0x202);
    assert_eq!(SESSION_TRUNCATED_OBJECT, 0x203);
    // (0x300-0x3FF は制御ストリーム / 制御メッセージ)
    assert_eq!(SESSION_MALFORMED_MESSAGE, 0x300);
    assert_eq!(SESSION_INVALID_FIELD, 0x301);
    assert_eq!(SESSION_INVALID_TRACK_NAMESPACE, 0x302);
    assert_eq!(SESSION_UNEXPECTED_MESSAGE, 0x303);
    assert_eq!(SESSION_ROLE_VIOLATION, 0x304);
    assert_eq!(SESSION_INVALID_PARAMETER, 0x305);
    assert_eq!(SESSION_CLOSED_CONTROL_STREAM, 0x306);
    // (0x400-0x4FF は双方で発生しうるエラー)
    assert_eq!(SESSION_DELTA_ENCODING_OVERFLOW, 0x400);
    assert_eq!(SESSION_FIELD_LENGTH_EXCEEDED, 0x401);
}

/// §16.12.2 (REQUEST_ERROR Codes) と一致する
#[test]
fn request_error_codes_match_iana_registry() {
    assert_eq!(REQUEST_INTERNAL_ERROR, 0x0);
    assert_eq!(REQUEST_UNAUTHORIZED, 0x1);
    assert_eq!(REQUEST_TIMEOUT, 0x2);
    assert_eq!(REQUEST_NOT_SUPPORTED, 0x3);
    assert_eq!(REQUEST_MALFORMED_AUTH_TOKEN, 0x4);
    assert_eq!(REQUEST_EXPIRED_AUTH_TOKEN, 0x5);
    assert_eq!(REQUEST_GOING_AWAY, 0x6);
    assert_eq!(REQUEST_EXCESSIVE_LOAD, 0x9);
    assert_eq!(REQUEST_DOES_NOT_EXIST, 0x10);
    assert_eq!(REQUEST_INVALID_RANGE, 0x11);
    assert_eq!(REQUEST_MALFORMED_TRACK, 0x12);
    assert_eq!(REQUEST_UNINTERESTED, 0x20);
    assert_eq!(REQUEST_PREFIX_OVERLAP, 0x30);
    assert_eq!(REQUEST_NAMESPACE_TOO_LARGE, 0x31);
    assert_eq!(REQUEST_UNSUPPORTED_EXTENSION, 0x33);
    assert_eq!(REQUEST_REDIRECT, 0x34);
    assert_eq!(REQUEST_CONFLICTING_FILTERS, 0x35);
    assert_eq!(REQUEST_INVALID_FILTER, 0x36);
}

/// §16.12.3 (PUBLISH_DONE Codes) と一致する
#[test]
fn publish_done_codes_match_iana_registry() {
    assert_eq!(PUBLISH_DONE_INTERNAL_ERROR, 0x0);
    assert_eq!(PUBLISH_DONE_UNAUTHORIZED, 0x1);
    assert_eq!(PUBLISH_DONE_TRACK_ENDED, 0x2);
    assert_eq!(PUBLISH_DONE_GOING_AWAY, 0x4);
    assert_eq!(PUBLISH_DONE_TOO_FAR_BEHIND, 0x5);
    assert_eq!(PUBLISH_DONE_EXPIRED, 0x6);
    assert_eq!(PUBLISH_DONE_UPDATE_FAILED, 0x8);
    assert_eq!(PUBLISH_DONE_EXCESSIVE_LOAD, 0x9);
    assert_eq!(PUBLISH_DONE_MALFORMED_TRACK, 0x12);
}

/// §16.12.4 (Stream Reset Error Codes) と一致する
#[test]
fn stream_reset_codes_match_iana_registry() {
    assert_eq!(STREAM_INTERNAL_ERROR, 0x0);
    assert_eq!(STREAM_CANCELLED, 0x1);
    assert_eq!(STREAM_DELIVERY_TIMEOUT, 0x2);
    assert_eq!(STREAM_SESSION_CLOSED, 0x3);
    assert_eq!(STREAM_GOING_AWAY, 0x4);
    assert_eq!(STREAM_TOO_FAR_BEHIND, 0x5);
    assert_eq!(STREAM_UNKNOWN_OBJECT_STATUS, 0x6);
    assert_eq!(STREAM_EXPIRED_AUTH_TOKEN, 0x7);
    assert_eq!(STREAM_EXCESSIVE_LOAD, 0x9);
    assert_eq!(STREAM_MALFORMED_TRACK, 0x12);
}

/// `MessageError` を `Box<dyn std::error::Error + Send + Sync>` へ `?` で変換できる
///
/// codec のエラーを自身のエラー型に包んで伝播させる利用側が `?` を使えることを確認する。
#[test]
fn message_error_converts_into_boxed_error() {
    /// `MessageError` を返す公開 API を `?` で伝播させる
    fn decode_empty() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let _ = ControlMessage::decode(&[])?;
        Ok(())
    }

    let err = decode_empty().expect_err("空のバイト列はデコードに失敗すること");
    assert_eq!(
        err.to_string(),
        "unexpected end of buffer",
        "Display が失敗の内容を表すこと"
    );
    assert!(err.source().is_none(), "内包するエラーは無いこと");
}

mod unknown_error_code_normalization {
    use shiguredo_moqt::error::{
        PUBLISH_DONE_INTERNAL_ERROR, PUBLISH_DONE_TRACK_ENDED, REQUEST_INTERNAL_ERROR,
        REQUEST_MALFORMED_TRACK, SESSION_AUTH_TOKEN_CACHE_OVERFLOW, SESSION_CLOSED_CONTROL_STREAM,
        SESSION_CONTROL_MESSAGE_TIMEOUT, SESSION_DATA_STREAM_TIMEOUT,
        SESSION_DELTA_ENCODING_OVERFLOW, SESSION_DUPLICATE_AUTH_TOKEN_ALIAS,
        SESSION_DUPLICATE_TRACK_ALIAS, SESSION_EXPIRED_AUTH_TOKEN, SESSION_FIELD_LENGTH_EXCEEDED,
        SESSION_GOAWAY_TIMEOUT, SESSION_INTERNAL_ERROR, SESSION_INVALID_AUTHORITY,
        SESSION_INVALID_FIELD, SESSION_INVALID_OBJECT_STATUS, SESSION_INVALID_PARAMETER,
        SESSION_INVALID_PATH, SESSION_INVALID_REQUEST_ID, SESSION_INVALID_STREAM_HEADER,
        SESSION_INVALID_TRACK_NAMESPACE, SESSION_MALFORMED_AUTH_TOKEN, SESSION_MALFORMED_AUTHORITY,
        SESSION_MALFORMED_KEY_VALUE, SESSION_MALFORMED_MESSAGE, SESSION_MALFORMED_OBJECT,
        SESSION_MALFORMED_PATH, SESSION_NO_ERROR, SESSION_PROTOCOL_VIOLATION,
        SESSION_ROLE_VIOLATION, SESSION_TOO_MANY_REQUEST_UPDATES, SESSION_TRUNCATED_OBJECT,
        SESSION_UNAUTHORIZED, SESSION_UNEXPECTED_MESSAGE, SESSION_UNKNOWN_AUTH_TOKEN_ALIAS,
        STREAM_CANCELLED, STREAM_INTERNAL_ERROR, normalize_publish_done_code,
        normalize_request_error_code, normalize_session_termination_code,
        normalize_stream_reset_code,
    };

    /// 既知コードはそのまま返ること (4 文脈)
    #[test]
    fn known_codes_are_unchanged() {
        assert_eq!(
            normalize_session_termination_code(SESSION_NO_ERROR),
            SESSION_NO_ERROR
        );
        assert_eq!(
            normalize_request_error_code(REQUEST_MALFORMED_TRACK),
            REQUEST_MALFORMED_TRACK
        );
        assert_eq!(
            normalize_publish_done_code(PUBLISH_DONE_TRACK_ENDED),
            PUBLISH_DONE_TRACK_ENDED
        );
        assert_eq!(
            normalize_stream_reset_code(STREAM_CANCELLED),
            STREAM_CANCELLED
        );
    }

    /// draft-ietf-moq-transport-23 §12.2 / §16.12.1 の全コードが既知として扱われること
    ///
    /// 正規化でこれらが INTERNAL_ERROR に落ちると、受信側のアプリが原因を失う。
    /// レジストリ表 (Table 20) に載る全コードと、コード表で定義された 0x6 を網羅する。
    #[test]
    fn all_defined_session_codes_are_known() {
        for code in [
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
        ] {
            assert_eq!(
                normalize_session_termination_code(code),
                code,
                "コード {code:#x} が既知として保たれること"
            );
        }
    }

    /// 隣接する未割り当て値は未知コードとして INTERNAL_ERROR に正規化されること
    ///
    /// 定義済みコードの集合を広げた後も、未割り当て値 (0x7 / 0x1C / 0x204 / 0x307 /
    /// 0x402) の扱いが変わらないことを固定する。
    #[test]
    fn unassigned_session_codes_are_normalized_to_internal_error() {
        for code in [0x7u64, 0x1C, 0x204, 0x307, 0x402] {
            assert_eq!(
                normalize_session_termination_code(code),
                SESSION_INTERNAL_ERROR,
                "未割り当てのコード {code:#x} は INTERNAL_ERROR に正規化されること"
            );
        }
    }

    /// 未知コードは文脈ごとの INTERNAL_ERROR 相当になること (4 文脈)
    #[test]
    fn unknown_codes_are_normalized_to_internal_error() {
        // どのレジストリにも存在しない値を使う (Session 0x7 / REQUEST 0x7 /
        // PUBLISH_DONE 0x3 / Stream 0x8)
        assert_eq!(
            normalize_session_termination_code(0x7),
            SESSION_INTERNAL_ERROR
        );
        assert_eq!(normalize_request_error_code(0x7), REQUEST_INTERNAL_ERROR);
        assert_eq!(
            normalize_publish_done_code(0x3),
            PUBLISH_DONE_INTERNAL_ERROR
        );
        assert_eq!(normalize_stream_reset_code(0x8), STREAM_INTERNAL_ERROR);
    }

    /// GREASE 値も文脈ごとの INTERNAL_ERROR 相当になること
    #[test]
    fn grease_codes_are_normalized_to_internal_error() {
        let grease = shiguredo_moqt::grease::generate(13).expect("GREASE 値を生成できること");
        assert!(shiguredo_moqt::grease::is_grease(grease));
        assert_eq!(
            normalize_session_termination_code(grease),
            SESSION_INTERNAL_ERROR
        );
        assert_eq!(normalize_request_error_code(grease), REQUEST_INTERNAL_ERROR);
        assert_eq!(
            normalize_publish_done_code(grease),
            PUBLISH_DONE_INTERNAL_ERROR
        );
        assert_eq!(normalize_stream_reset_code(grease), STREAM_INTERNAL_ERROR);
    }
}

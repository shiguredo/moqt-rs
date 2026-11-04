//! Track / Object / LOC の Property が共有する値の検証
//!
//! draft-ietf-moq-loc-05 §2.3.1.3 (Codec String) の CODEC_STRING (0x11) は LOC が定義する
//! Property だが、Scope が Track と Object の両方である (同 §6.1 (MOQ Properties Registry)
//! Table 1)。そのため LOC 側 (`crate::loc`) と transport 側 (`crate::track_properties`) の
//! 両方が同じ値の検証を行う。Object 側の `crate::object_properties` も同じ値を検証するため、
//! 検証規則と Property Type を 1 箇所に保つため本モジュールに置く。
//!
//! 本モジュールは値の検証だけを持ち、KVP の走査やエンコーディングは持たない。

use crate::error::MessageError;

/// CODEC_STRING の Property Type
///
/// draft-ietf-moq-loc-05 §2.3.1.3 (Codec String): ID=0x11。
/// draft-ietf-moq-loc-05 §6.1 (MOQ Properties Registry) Table 1: Scope=Track, Object。
///
/// この定義は draft 由来であり、将来の draft 改版で変わる可能性がある。
pub(crate) const CODEC_STRING_TYPE: u64 = 0x11;

/// CODEC_STRING (0x11) の値が UTF-8 文字列として解釈できることを検証する
///
/// draft-ietf-moq-loc-05 §2.3.1.3 (Codec String) は、値が WebCodecs Codec Registry の
/// codec 値の UTF-8 文字列であると定める。draft-ietf-moq-transport-23 §8.3
/// (Key-Value-Pair Structure) は、受信側が Type を理解する場合に Length/Value が定義された
/// serialization と一致しなければ MALFORMED_KEY_VALUE でセッションを閉じることを MUST と
/// する。送信側も同じ検証を行い、不正な UTF-8 を送出しない。
///
/// LOC / Track / Object の 3 者の Properties から呼び、エラー分類とメッセージを 1 箇所に
/// 揃える。
/// 専用のパーサーは持たず、CODEC_STRING の値を得た後の検証だけを行う。
pub(crate) fn validate_codec_string_utf8(bytes: &[u8]) -> Result<&str, MessageError> {
    core::str::from_utf8(bytes).map_err(|_| {
        MessageError::MalformedKeyValue(
            "CODEC_STRING value is not valid UTF-8 (draft-ietf-moq-loc-05 §2.3.1.3)",
        )
    })
}

//! draft-ietf-moq-transport-21 §13 (Grease) に準拠した GREASE ユーティリティ
//!
//! GREASE (Generate Random Extensions And Sustain Extensibility) は、
//! 未知値の無視が実装されていることを確認し将来の拡張互換性を確保するための
//! 予約値である。詳細は RFC 9170 Section 3.3 を参照。
//!
//! # 値パターン
//!
//! `0x7F * N + 0x9D` (N は非負整数)。上限は draft-ietf-moq-transport-21 §13 (Grease) が示す `0x3FFFFFFFFFFFFFDE`
//! (62-bit varint の範囲に収まる値)。
//!
//! # 対象レジストリ (draft-ietf-moq-transport-21 §13 (Grease))
//!
//! - Setup Options (draft-ietf-moq-transport-21 §16.4 (Setup Options))
//! - Properties (draft-ietf-moq-transport-21 §16.8 (Properties))
//! - Session Termination Error Codes (draft-ietf-moq-transport-21 §16.11.1 (Session Termination Error Codes))
//! - REQUEST_ERROR Codes (draft-ietf-moq-transport-21 §16.11.2 (REQUEST_ERROR Codes))
//! - PUBLISH_DONE Codes (draft-ietf-moq-transport-21 §16.11.3 (PUBLISH_DONE Codes))
//! - Stream Reset Error Codes (draft-ietf-moq-transport-21 §16.11.4 (Stream Reset Error Codes))
//! - MOQT Auth Token Type
//!
//! # 注意
//!
//! 本計算式は draft 由来であり、将来の改訂で base/interval/upper bound が
//! 変更される可能性がある (draft-ietf-moq-transport-21 §13 (Grease) に計算式が明示)。
//!
//! # 例
//!
//! ```
//! use shiguredo_moqt::grease::{generate, is_grease};
//!
//! let v = generate(0).expect("テストフィクスチャの前提条件を満たす");
//! assert_eq!(v, 0x9D);
//! assert!(is_grease(v));
//! assert!(is_grease(generate(42).expect("テストフィクスチャの前提条件を満たす")));
//! assert!(!is_grease(0x9C));
//! ```

/// GREASE 値の基数 (draft-ietf-moq-transport-21 §13 (Grease) "0x7f * N + 0x9D")
pub const GREASE_BASE: u64 = 0x9D;

/// GREASE 値の間隔 (draft-ietf-moq-transport-21 §13 (Grease) "0x7f * N + 0x9D")
pub const GREASE_INTERVAL: u64 = 0x7F;

/// GREASE 値の上限 (draft-ietf-moq-transport-21 §13 (Grease) "0x3fffffffffffffde")
///
/// `0x7F * N + 0x9D` の最大値。生成値はこれを超えてはならない。
pub const GREASE_MAX: u64 = 0x3FFF_FFFF_FFFF_FFDE;

/// GREASE 値を生成する
///
/// `0x7F * n + 0x9D` を返す。結果が `GREASE_MAX` を超える場合、または
/// u64 オーバーフローする場合は `None` を返す。
pub fn generate(n: u64) -> Option<u64> {
    let value = GREASE_INTERVAL.checked_mul(n)?.checked_add(GREASE_BASE)?;
    if value > GREASE_MAX {
        return None;
    }
    Some(value)
}

/// 与えられた値が GREASE 値かどうかを判定する
///
/// `value >= 0x9D` かつ `(value - 0x9D) % 0x7F == 0` を満たすとき true。
/// 上限 `GREASE_MAX` を超えた値も、パターンに合致すれば true を返す
/// (受信側は将来の draft で上限が広がった場合にも未知値として握りつぶせるため)。
pub fn is_grease(value: u64) -> bool {
    if value < GREASE_BASE {
        return false;
    }
    (value - GREASE_BASE).is_multiple_of(GREASE_INTERVAL)
}

//! 受信側 / 配信側の catch_up で共有する上限の式と定数
//!
//! 受信側 (`AudioReceiveCatchUp`) と配信側 (`AudioPublishCatchUp`) は、どちらも
//! 「健全な遅れ (床) + 増加量」と絶対下限の大きい方を上限にする。値と式を 1 箇所に置き、
//! 片方だけを直す事故を防ぐ。床の計算と開始・終了の判定は用途ごとに意味が異なるため、
//! ここでは共有しない (各モジュールに残す)。

/// 遅れの上限の絶対下限 (ミリ秒)
pub(crate) const CATCH_UP_MIN_MS: i64 = 60;

/// 床からの増加量 (ミリ秒)
pub(crate) const CATCH_UP_GROWTH_MS: i64 = 40;

/// 上限を超えた状態が続いたとみなす時間 (ミリ秒)
pub(crate) const CATCH_UP_CONFIRM_MS: i64 = 100;

/// 追いつきを始めた後のクールダウン (ミリ秒)
pub(crate) const CATCH_UP_COOLDOWN_MS: i64 = 1_000;

/// ミリ秒をマイクロ秒へ変換する (飽和)
pub(crate) const fn ms_to_us(ms: i64) -> i64 {
    ms.saturating_mul(1_000)
}

/// ミリ秒単位の遅れの上限
///
/// `max(min_ms, floor_ms + growth_ms)` である。
pub(crate) fn limit_ms(floor_ms: i64, min_ms: i64, growth_ms: i64) -> i64 {
    min_ms.max(floor_ms.saturating_add(growth_ms))
}

/// マイクロ秒単位の遅れの上限
///
/// 受信側はマイクロ秒で計算するため、ミリ秒の定数を変換して同じ式を通す。
pub(crate) fn limit_us(floor_us: i64, min_ms: i64, growth_ms: i64) -> i64 {
    ms_to_us(min_ms).max(floor_us.saturating_add(ms_to_us(growth_ms)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 床が小さければ絶対下限が上限になること
    #[test]
    fn limit_uses_absolute_floor() {
        assert_eq!(limit_ms(0, CATCH_UP_MIN_MS, CATCH_UP_GROWTH_MS), 60);
        assert_eq!(limit_ms(10, CATCH_UP_MIN_MS, CATCH_UP_GROWTH_MS), 60);
    }

    /// 床が大きければ床 + 増加量が上限になること
    #[test]
    fn limit_uses_floor_plus_growth() {
        assert_eq!(limit_ms(100, CATCH_UP_MIN_MS, CATCH_UP_GROWTH_MS), 140);
        assert_eq!(
            limit_us(100_000, CATCH_UP_MIN_MS, CATCH_UP_GROWTH_MS),
            140_000
        );
    }
}

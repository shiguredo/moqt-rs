#![no_std]

//! Happy Eyeballs v3 の接続候補スケジューラ
//!
//! draft-ietf-happy-happyeyeballs-v3-05 のうち、接続候補の並びと接続試行の開始時刻を
//! 決める純粋な状態機械を提供する。I/O、DNS、ソケット、タイマー、非同期タスクは扱わない。
//! 呼び出し側が単調増加の時刻 (ミリ秒) と試行結果を渡し、状態機械が次に開始する候補や
//! 次回開始時刻を返す。
//!
//! - §5.3 (Sorting Destination Endpoints Within Groups): 宛先アドレスをアドレスファミリで
//!   交互に並べる。本実装は解決器が返した順序をファミリ内の優先順として使い、先頭の
//!   アドレスのファミリを優先する (Preferred Address Family Count は既定 1、設定可能)
//! - §6 (Connection Attempts): 最初の候補を即座に開始し、以降は Connection Attempt Delay
//!   (既定 250 ms、最小 100 ms、最大 2 s) ごとに開始する。試行の成功率を上げるため、
//!   試行が失敗したときは次の候補の開始を前倒しする。ただし、直前の試行から
//!   `minimum_connection_attempt_delay` (既定 100 ms、仕様の MUST である 10 ms を下限とする)
//!   未満では開始しない
//! - §6.1 (Determining successful connection establishment): 成功判定は呼び出し側が行う。
//!   状態機械は試行の成功・失敗を受け取り、最初の成功を勝者として返す
//!
//! DNS の非同期解決 (§4) と SVCB/HTTPS レコードの解釈 (§5.1 / §5.2 / §6.2 / §6.3) は
//! 対象外である。候補のトランスポート種別とアドレスは呼び出し側が決める。

extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::vec::Vec;
use core::net::SocketAddr;
use core::time::Duration;

pub mod scheduler;

/// 連続する接続試行の開始間隔の絶対下限
///
/// draft-ietf-happy-happyeyeballs-v3-05 §6: "a subsequent connection MUST NOT be started
/// within 10 milliseconds of the previous attempt." 失敗による前倒しでもこの下限は破らない。
pub const HARD_MINIMUM_CONNECTION_ATTEMPT_DELAY: Duration = Duration::from_millis(10);

/// 設定できる Connection Attempt Delay の安全上限
///
/// 時刻の加算でオーバーフローしないための実装上の上限であり、draft-05 の推奨最大値
/// (2 秒) より十分に大きい。60 秒を超える値は 60 秒に丸める。
const MAXIMUM_ALLOWED_CONNECTION_ATTEMPT_DELAY: Duration = Duration::from_secs(60);

/// Happy Eyeballs v3 の設定
///
/// draft-ietf-happy-happyeyeballs-v3-05 §10 (Summary of Configurable Values) の値を
/// 保持する。既定値は同節の推奨値である。仕様の範囲外の値と、実装の安全上限
/// (60 秒) を超える値は [`Self::sanitized`] で丸める。
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Connection Attempt Delay (§6 / §10)。既定 250 ms
    ///
    /// 次の候補を開始するまでの基準の待ち時間。実際の値は
    /// [`Self::minimum_connection_attempt_delay`] と
    /// [`Self::maximum_connection_attempt_delay`] の範囲に丸められる。
    pub connection_attempt_delay: Duration,
    /// Connection Attempt Delay の推奨最小値 (§6 / §10)。既定 100 ms
    ///
    /// 仕様の MUST である 10 ms より小さい値は
    /// [`HARD_MINIMUM_CONNECTION_ATTEMPT_DELAY`] に切り上げる。
    pub minimum_connection_attempt_delay: Duration,
    /// Connection Attempt Delay の推奨最大値 (§6 / §10)。既定 2 秒
    pub maximum_connection_attempt_delay: Duration,
    /// Connection Attempt Delay Multiplier (§6)。既定 1.0
    ///
    /// 進行中の試行が増えるたびにこの係数を累乗する。1.0 で一定、1.0 未満と有限でない
    /// 値は 1.0 に切り上げる。
    pub connection_attempt_delay_multiplier: f64,
    /// Preferred Address Family Count (§5.3)。既定 1
    ///
    /// 先頭のアドレスファミリを連続して試す数。0 は 1 に切り上げる。
    pub preferred_address_family_count: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            connection_attempt_delay: Duration::from_millis(250),
            minimum_connection_attempt_delay: Duration::from_millis(100),
            maximum_connection_attempt_delay: Duration::from_secs(2),
            connection_attempt_delay_multiplier: 1.0,
            preferred_address_family_count: 1,
        }
    }
}

impl Config {
    /// 仕様の下限・上限に合わせて値を丸める
    ///
    /// 利用者が範囲外の値を設定しても、競争の途中で panic しないようにする。丸めた
    /// 結果は [`Self`] を返し、呼び出し側は競争の開始時に 1 度だけ使う。
    pub fn sanitized(mut self) -> Self {
        self.minimum_connection_attempt_delay = self
            .minimum_connection_attempt_delay
            .max(HARD_MINIMUM_CONNECTION_ATTEMPT_DELAY)
            .min(MAXIMUM_ALLOWED_CONNECTION_ATTEMPT_DELAY);
        self.maximum_connection_attempt_delay = self
            .maximum_connection_attempt_delay
            .max(self.minimum_connection_attempt_delay)
            .min(MAXIMUM_ALLOWED_CONNECTION_ATTEMPT_DELAY);
        self.connection_attempt_delay = self.connection_attempt_delay.clamp(
            self.minimum_connection_attempt_delay,
            self.maximum_connection_attempt_delay,
        );
        if !self.connection_attempt_delay_multiplier.is_finite()
            || self.connection_attempt_delay_multiplier < 1.0
        {
            self.connection_attempt_delay_multiplier = 1.0;
        }
        self.preferred_address_family_count = self.preferred_address_family_count.max(1);
        self
    }
}

/// 同じトランスポートで試すアドレスのグループ
///
/// 呼び出し側はトランスポートの優先順にグループを並べる。グループ内のアドレスは
/// `order_candidates` がアドレスファミリ交互の順序へ並べ替える。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateGroup<C> {
    /// 接続経路を表す呼び出し側の値
    pub transport: C,
    /// その経路で試す接続先アドレス
    pub addrs: Vec<SocketAddr>,
}

/// 1 回の接続試行で試す、トランスポート種別と接続先アドレスの組
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate<C> {
    /// 接続経路を表す呼び出し側の値
    pub transport: C,
    /// 接続先アドレス
    pub addr: SocketAddr,
}

/// 候補グループを接続試行の順序に並べる
///
/// グループの順序は呼び出し側が渡したトランスポート優先順を保つ。各グループ内では
/// アドレスをアドレスファミリ交互の順序にし、重複したアドレスを 1 つに畳む。
pub fn order_candidates<C: Copy>(
    groups: &[CandidateGroup<C>],
    config: &Config,
) -> Vec<Candidate<C>> {
    let mut candidates = Vec::new();
    for group in groups {
        for addr in
            interleave_address_families(&group.addrs, config.preferred_address_family_count.max(1))
        {
            candidates.push(Candidate {
                transport: group.transport,
                addr,
            });
        }
    }
    candidates
}

/// アドレスをアドレスファミリ交互の順序に並べる
///
/// 先頭のアドレスのファミリを優先し、`preferred_count` 個ずつ取り出したあと、もう一方の
/// ファミリから 1 個取り出す。同じアドレスの重複は除く。ファミリ内の順序は解決器が
/// 返した順序をそのまま使う (RFC 6724 の宛先アドレス選択は解決器の責務とする)。
pub fn interleave_address_families(
    addrs: &[SocketAddr],
    preferred_count: usize,
) -> Vec<SocketAddr> {
    // 同じアドレスが複数回返ることがあるため、最初の出現だけを残す
    let mut unique = Vec::new();
    for &addr in addrs {
        if !unique.contains(&addr) {
            unique.push(addr);
        }
    }
    let Some(first) = unique.first().copied() else {
        return Vec::new();
    };
    let prefer_ipv6 = first.is_ipv6();

    let mut preferred = Vec::new();
    let mut other = Vec::new();
    for addr in unique {
        if addr.is_ipv6() == prefer_ipv6 {
            preferred.push(addr);
        } else {
            other.push(addr);
        }
    }

    let mut ordered = Vec::new();
    let mut preferred_iter = preferred.into_iter();
    let mut other_iter = other.into_iter();
    loop {
        let mut started = false;
        for _ in 0..preferred_count.max(1) {
            match preferred_iter.next() {
                Some(addr) => {
                    ordered.push(addr);
                    started = true;
                }
                None => break,
            }
        }
        if let Some(addr) = other_iter.next() {
            ordered.push(addr);
            started = true;
        }
        if !started {
            break;
        }
    }
    ordered
}

/// 進行中の試行数から次の Connection Attempt Delay を決める
///
/// §6 の Connection Attempt Delay Multiplier を適用し、推奨最小値と推奨最大値で挟む。
/// 乗算は `f64` で行い、極端な設定でも時刻計算がオーバーフローしないよう上限で飽和させる。
/// `config` は [`Config::sanitized`] 済みであること。
pub fn next_attempt_delay(in_flight: usize, config: &Config) -> Duration {
    let exponent = u32::try_from(in_flight.saturating_sub(1))
        .unwrap_or(u32::MAX)
        .min(31);
    // no_std の core には f64::powi が無いため、指数的に掛ける。指数は 31 以下に
    // 制限しており、乗算結果が無限大になっても後段の上限比較で飽和する
    let mut factor = 1.0f64;
    for _ in 0..exponent {
        factor *= config.connection_attempt_delay_multiplier;
    }
    let maximum = config.maximum_connection_attempt_delay.as_secs_f64();
    let seconds = (config.connection_attempt_delay.as_secs_f64() * factor).min(maximum);
    let delay =
        Duration::try_from_secs_f64(seconds).unwrap_or(config.maximum_connection_attempt_delay);
    delay.max(config.minimum_connection_attempt_delay)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use core::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn v4(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    }

    fn v6(port: u16) -> SocketAddr {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
    }

    /// 既定値は draft-05 §10 の推奨値である
    #[test]
    fn default_config_matches_draft_values() {
        let config = Config::default();
        assert_eq!(config.connection_attempt_delay, Duration::from_millis(250));
        assert_eq!(
            config.minimum_connection_attempt_delay,
            Duration::from_millis(100)
        );
        assert_eq!(
            config.maximum_connection_attempt_delay,
            Duration::from_secs(2)
        );
        assert_eq!(config.connection_attempt_delay_multiplier, 1.0);
        assert_eq!(config.preferred_address_family_count, 1);
    }

    /// 範囲外の設定値は仕様の範囲に丸める
    #[test]
    fn config_sanitizes_out_of_range_values() {
        let config = Config {
            connection_attempt_delay: Duration::from_millis(1),
            minimum_connection_attempt_delay: Duration::from_millis(1),
            maximum_connection_attempt_delay: Duration::from_millis(0),
            connection_attempt_delay_multiplier: 0.5,
            preferred_address_family_count: 0,
        }
        .sanitized();
        assert_eq!(
            config.minimum_connection_attempt_delay,
            HARD_MINIMUM_CONNECTION_ATTEMPT_DELAY
        );
        assert_eq!(
            config.maximum_connection_attempt_delay,
            HARD_MINIMUM_CONNECTION_ATTEMPT_DELAY
        );
        assert_eq!(
            config.connection_attempt_delay,
            HARD_MINIMUM_CONNECTION_ATTEMPT_DELAY
        );
        assert_eq!(config.connection_attempt_delay_multiplier, 1.0);
        assert_eq!(config.preferred_address_family_count, 1);
    }

    /// 極端な Duration と倍率でも panic しない
    #[test]
    fn config_caps_extreme_values() {
        for multiplier in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let config = Config {
                connection_attempt_delay: Duration::MAX,
                minimum_connection_attempt_delay: Duration::MAX,
                maximum_connection_attempt_delay: Duration::MAX,
                connection_attempt_delay_multiplier: multiplier,
                preferred_address_family_count: usize::MAX,
            }
            .sanitized();
            assert_eq!(
                config.minimum_connection_attempt_delay,
                Duration::from_secs(60)
            );
            assert_eq!(
                config.maximum_connection_attempt_delay,
                Duration::from_secs(60)
            );
            assert_eq!(config.connection_attempt_delay, Duration::from_secs(60));
            assert_eq!(config.connection_attempt_delay_multiplier, 1.0);
        }
    }

    /// 先頭のファミリを 1 個ずつ交互に並べる
    #[test]
    fn interleave_address_families_alternates() {
        let addrs = [v6(1), v6(2), v4(3), v4(4)];
        assert_eq!(
            interleave_address_families(&addrs, 1),
            vec![v6(1), v4(3), v6(2), v4(4)]
        );
    }

    /// Preferred Address Family Count が 2 なら先頭ファミリを 2 個ずつ取る
    #[test]
    fn interleave_address_families_uses_preferred_count() {
        let addrs = [v6(1), v6(2), v6(3), v4(4), v4(5)];
        assert_eq!(
            interleave_address_families(&addrs, 2),
            vec![v6(1), v6(2), v4(4), v6(3), v4(5)]
        );
    }

    /// 同じアドレスの重複を取り除く
    #[test]
    fn interleave_address_families_removes_duplicates() {
        let addrs = [v4(1), v4(1), v6(2), v6(2), v4(3)];
        assert_eq!(
            interleave_address_families(&addrs, 1),
            vec![v4(1), v6(2), v4(3)]
        );
    }

    /// 1 ファミリだけなら順序を変えない
    #[test]
    fn interleave_address_families_keeps_single_family_order() {
        let addrs = [v4(1), v4(2), v4(3)];
        assert_eq!(interleave_address_families(&addrs, 1), addrs);
    }

    /// トランスポートの優先順ごとに候補を並べる
    #[test]
    fn order_candidates_groups_by_transport() {
        let groups = vec![
            CandidateGroup {
                transport: 0u8,
                addrs: vec![v6(1), v4(2)],
            },
            CandidateGroup {
                transport: 1u8,
                addrs: vec![v6(3), v4(4)],
            },
        ];
        let candidates = order_candidates(&groups, &Config::default());
        assert_eq!(
            candidates,
            vec![
                Candidate {
                    transport: 0,
                    addr: v6(1)
                },
                Candidate {
                    transport: 0,
                    addr: v4(2)
                },
                Candidate {
                    transport: 1,
                    addr: v6(3)
                },
                Candidate {
                    transport: 1,
                    addr: v4(4)
                },
            ]
        );
    }

    /// 倍率を適用し、最大値で飽和する
    #[test]
    fn next_attempt_delay_applies_multiplier() {
        let config = Config {
            connection_attempt_delay: Duration::from_millis(100),
            minimum_connection_attempt_delay: Duration::from_millis(100),
            maximum_connection_attempt_delay: Duration::from_secs(2),
            connection_attempt_delay_multiplier: 2.0,
            ..Config::default()
        }
        .sanitized();
        assert_eq!(next_attempt_delay(1, &config), Duration::from_millis(100));
        assert_eq!(next_attempt_delay(2, &config), Duration::from_millis(200));
        assert_eq!(next_attempt_delay(5, &config), Duration::from_millis(1600));
        assert_eq!(next_attempt_delay(6, &config), Duration::from_secs(2));
    }

    /// 基準値が最小値を下回る場合は最小値に切り上げる
    #[test]
    fn next_attempt_delay_raises_base_below_minimum() {
        let config = Config {
            connection_attempt_delay: Duration::from_millis(20),
            minimum_connection_attempt_delay: Duration::from_millis(100),
            maximum_connection_attempt_delay: Duration::from_secs(2),
            ..Config::default()
        }
        .sanitized();
        assert_eq!(next_attempt_delay(1, &config), Duration::from_millis(100));
    }
}

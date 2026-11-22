//! Happy Eyeballs v3 の Sans I/O 候補整列と設定の PBT

use std::collections::BTreeSet;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use happy_eyeballs::{Config, interleave_address_families};
use pbt::common::test_runner;

/// 生成するアドレス数の上限
const MAX_ADDRESSES: u64 = 8;

/// アドレスファミリの交互配置は入力を失わず、重複を除いた集合と一致する
#[test]
fn interleave_preserves_the_unique_addresses() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let count = noprop::sample_u64_in(ctx, 0..=MAX_ADDRESSES);
        let mut addrs = Vec::new();
        for _ in 0..count {
            let port = u16::try_from(noprop::sample_u64_in(ctx, 0..=65535))
                .expect("サンプラーは u16 の範囲内の値を返す");
            let addr = if noprop::sample_bool(ctx) {
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
            } else {
                SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port)
            };
            addrs.push(addr);
        }
        let preferred_count = usize::try_from(noprop::sample_u64_in(ctx, 0..=4))
            .expect("サンプラーは usize の範囲内の値を返す");

        let ordered = interleave_address_families(&addrs, preferred_count);
        let expected: BTreeSet<SocketAddr> = addrs.iter().copied().collect();
        let actual: BTreeSet<SocketAddr> = ordered.iter().copied().collect();

        assert_eq!(actual, expected, "アドレスの集合が変わらないこと");
        assert_eq!(
            ordered.len(),
            actual.len(),
            "同じアドレスを重複して出力しないこと"
        );
        if let Some(first) = addrs.first() {
            assert_eq!(
                ordered.first(),
                Some(first),
                "先頭のアドレスファミリを優先すること"
            );
        }
        Ok(())
    })?;
    Ok(())
}

/// 設定の正規化後は 10 ms 以上 60 秒以下で、最小値 <= 基準値 <= 最大値になる
#[test]
fn sanitized_config_stays_within_the_safety_bounds() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let config = Config {
            connection_attempt_delay: std::time::Duration::from_millis(noprop::sample_u64_in(
                ctx,
                0..=120_000,
            )),
            minimum_connection_attempt_delay: std::time::Duration::from_millis(
                noprop::sample_u64_in(ctx, 0..=120_000),
            ),
            maximum_connection_attempt_delay: std::time::Duration::from_millis(
                noprop::sample_u64_in(ctx, 0..=120_000),
            ),
            connection_attempt_delay_multiplier: if noprop::sample_bool(ctx) { 0.5 } else { 2.0 },
            preferred_address_family_count: usize::try_from(noprop::sample_u64_in(ctx, 0..=4))
                .expect("サンプラーは usize の範囲内の値を返す"),
        }
        .sanitized();

        assert!(config.minimum_connection_attempt_delay >= std::time::Duration::from_millis(10));
        assert!(config.minimum_connection_attempt_delay <= std::time::Duration::from_secs(60));
        assert!(
            config.maximum_connection_attempt_delay <= std::time::Duration::from_secs(60),
            "最大値は安全上限以下であること"
        );
        assert!(
            config.minimum_connection_attempt_delay <= config.connection_attempt_delay,
            "基準値は最小値以上であること"
        );
        assert!(
            config.connection_attempt_delay <= config.maximum_connection_attempt_delay,
            "基準値は最大値以下であること"
        );
        assert!(
            config.connection_attempt_delay_multiplier >= 1.0,
            "倍率は 1 以上であること"
        );
        assert!(
            config.preferred_address_family_count >= 1,
            "Preferred Address Family Count は 1 以上であること"
        );
        Ok(())
    })?;
    Ok(())
}

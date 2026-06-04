//! AuthTokenCache のラウンドトリップ系プロパティテスト
//!
//! - REGISTER → resolve のラウンドトリップ
//! - 重複 REGISTER の拒否
//! - DELETE 後の resolve 失敗
//! - max_size = 0 時の REGISTER 拒否

use pbt::common::test_runner;
use shiguredo_moqt::session::auth_token_cache::AuthTokenCache;

// AuthTokenCache: 十分な max_size の下で (alias, type, value) を REGISTER → resolve
#[test]
fn auth_token_cache_register_resolve() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64(ctx);
        let token_type = noprop::sample_u64(ctx);
        let value_len = noprop::sample_usize_in(ctx, 0..=128);
        let value = noprop::sample_bytes_vec(ctx, value_len);
        // 16 バイト + Value 分 + 余裕を持って max_size を設定
        let max_size = 16 + value.len() as u64 + 1024;
        let mut cache = AuthTokenCache::new(max_size);
        assert!(
            cache
                .try_register(alias, token_type, value.clone())
                .expect("テストフィクスチャの前提条件を満たす")
        );
        assert_eq!(cache.resolve(alias), Some((token_type, value.as_slice())));
        Ok(())
    })?;
    Ok(())
}

/// 同じ alias への 2 回目の REGISTER は常にエラー
#[test]
fn auth_token_cache_duplicate_register_fails() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64(ctx);
        let t1 = noprop::sample_u64(ctx);
        let t2 = noprop::sample_u64(ctx);
        let v1_len = noprop::sample_usize_in(ctx, 0..=32);
        let v1 = noprop::sample_bytes_vec(ctx, v1_len);
        let v2_len = noprop::sample_usize_in(ctx, 0..=32);
        let v2 = noprop::sample_bytes_vec(ctx, v2_len);
        let max_size = 16 * 2 + v1.len() as u64 + v2.len() as u64 + 1024;
        let mut cache = AuthTokenCache::new(max_size);
        assert!(
            cache
                .try_register(alias, t1, v1)
                .expect("テストフィクスチャの前提条件を満たす")
        );
        let result = cache.try_register(alias, t2, v2);
        assert!(result.is_err());
        Ok(())
    })?;
    Ok(())
}

/// DELETE 後は resolve が None を返す
#[test]
fn auth_token_cache_delete_forgets() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64(ctx);
        let token_type = noprop::sample_u64(ctx);
        let value_len = noprop::sample_usize_in(ctx, 0..=32);
        let value = noprop::sample_bytes_vec(ctx, value_len);
        let max_size = 16 + value.len() as u64 + 1024;
        let mut cache = AuthTokenCache::new(max_size);
        cache
            .try_register(alias, token_type, value)
            .expect("テストフィクスチャの前提条件を満たす");
        cache.delete(alias);
        assert!(cache.resolve(alias).is_none());
        assert_eq!(cache.total_size(), 0);
        Ok(())
    })?;
    Ok(())
}

/// max_size = 0 の場合は REGISTER は常に容量超過扱い (Ok(false))
/// draft-ietf-moq-transport-21 §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE): 未指定時のデフォルトは 0、alias 利用不可
#[test]
fn auth_token_cache_zero_limit_rejects_all() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let alias = noprop::sample_u64(ctx);
        let token_type = noprop::sample_u64(ctx);
        let value_len = noprop::sample_usize_in(ctx, 0..=32);
        let value = noprop::sample_bytes_vec(ctx, value_len);
        let mut cache = AuthTokenCache::new(0);
        let registered = cache
            .try_register(alias, token_type, value)
            .expect("テストフィクスチャの前提条件を満たす");
        assert!(!registered);
        assert_eq!(cache.len(), 0);
        Ok(())
    })?;
    Ok(())
}

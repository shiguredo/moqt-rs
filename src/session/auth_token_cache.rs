//! AUTHORIZATION_TOKEN Alias Cache
//!
//! draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter) / §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE) /
//! §9.1.4 (AUTHORIZATION TOKEN) に基づく。
//! draft 由来の実装のため将来変更される可能性がある。

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::error::SESSION_DUPLICATE_AUTH_TOKEN_ALIAS;

use super::types::SessionError;

/// AUTHORIZATION_TOKEN Alias Cache
///
/// draft-ietf-moq-transport-21 §9.20.3 (AUTHORIZATION TOKEN Parameter) に基づく。Client と Server は
/// それぞれ独立した alias 空間を持つため、各エンドポイントは自側が登録した
/// alias (相手がトラッキングすべき) と、相手が登録した alias (自側がトラッキングすべき)
/// の 2 つを保持する。
#[derive(Debug, Clone, Default)]
pub struct AuthTokenCache {
    entries: BTreeMap<u64, (u64, Vec<u8>)>,
    total_size: u64,
    max_size: u64,
}

impl AuthTokenCache {
    /// 新しいキャッシュを作成する
    ///
    /// `max_size` は Setup Option MAX_AUTH_TOKEN_CACHE_SIZE (draft §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE))。
    /// `max_size` が 0 の場合は Alias の登録は常に容量超過扱いとなる。
    pub fn new(max_size: u64) -> Self {
        Self {
            entries: BTreeMap::new(),
            total_size: 0,
            max_size,
        }
    }

    /// Token 1 つが占めるバイト数 (draft §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE): 16 バイト + Token Value のバイト数)
    fn entry_size(token_value_len: usize) -> u64 {
        16u64.saturating_add(token_value_len as u64)
    }

    /// REGISTER を試行する
    ///
    /// 戻り値:
    /// - `Ok(true)`: 登録成功
    /// - `Ok(false)`: キャッシュサイズ上限超過 (登録せず)
    /// - `Err`: alias 重複 (DUPLICATE_AUTH_TOKEN_ALIAS)
    pub fn try_register(
        &mut self,
        alias: u64,
        token_type: u64,
        token_value: Vec<u8>,
    ) -> Result<bool, SessionError> {
        if self.entries.contains_key(&alias) {
            return Err(SessionError::new(
                SESSION_DUPLICATE_AUTH_TOKEN_ALIAS,
                "duplicate AUTHORIZATION_TOKEN alias",
            ));
        }
        let entry_size = Self::entry_size(token_value.len());
        let new_total = self.total_size.saturating_add(entry_size);
        if new_total > self.max_size {
            return Ok(false);
        }
        self.entries.insert(alias, (token_type, token_value));
        self.total_size = new_total;
        Ok(true)
    }

    /// USE_ALIAS: 登録済みの alias を Token Type / Value に解決する
    ///
    /// draft §9.20.3 (AUTHORIZATION TOKEN Parameter): 未登録の alias を参照するとセッションエラー UNKNOWN_AUTH_TOKEN_ALIAS となるが、
    /// 本メソッドは単に `None` を返す。呼び出し側でエラーに変換する。
    pub fn resolve(&self, alias: u64) -> Option<(u64, &[u8])> {
        self.entries.get(&alias).map(|(t, v)| (*t, v.as_slice()))
    }

    /// DELETE: alias を除去する
    ///
    /// 未登録の alias を DELETE しようとした場合は単に no-op とする
    /// (draft §9.20.3 (AUTHORIZATION TOKEN Parameter): 登録は sender 側の管理責任であり、unknown alias の DELETE を
    /// セッションエラーにする規定はない)。
    pub fn delete(&mut self, alias: u64) {
        if let Some((_, v)) = self.entries.remove(&alias) {
            let removed = Self::entry_size(v.len());
            // 防御的に飽和減算する。通常運用では total_size >= removed のため
            // 飽和は発生しない。unknown alias の DELETE は no-op であり、
            // draft §9.1.3 の合計計算 (登録合計 - 削除合計) と整合する。
            self.total_size = self.total_size.saturating_sub(removed);
        }
    }

    /// 現在の登録エントリー数
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// キャッシュが空か
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 現在のキャッシュ合計サイズ (バイト)
    pub fn total_size(&self) -> u64 {
        self.total_size
    }

    /// キャッシュサイズ上限 (MAX_AUTH_TOKEN_CACHE_SIZE)
    pub fn max_size(&self) -> u64 {
        self.max_size
    }
}

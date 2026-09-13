//! AUTHORIZATION_TOKEN Alias Cache
//!
//! draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression) / §9.20.3 (AUTHORIZATION TOKEN Parameter) / §9.1.3 (MAX_AUTH_TOKEN_CACHE_SIZE) /
//! §9.1.4 (AUTHORIZATION TOKEN) に基づく。
//! draft 由来の実装のため将来変更される可能性がある。

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::error::SESSION_DUPLICATE_AUTH_TOKEN_ALIAS;

use super::types::SessionError;

/// AUTHORIZATION_TOKEN Alias Cache
///
/// draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression) に基づく。1 インスタンスは
/// 片方向の alias 空間を保持する。`Session::peer_auth_token_cache()` が返すのは
/// peer が REGISTER した alias を自側が保持する側であり、`max_size()` は自側 SETUP の
/// MAX_AUTH_TOKEN_CACHE_SIZE (受信側が自身のリソースを保護するために宣言する値) である。
/// peer SETUP を受信すると自側 SETUP の宣言値で確定し、受信前は 0 になる。
/// 自側が REGISTER した alias を peer が保持できる上限は
/// `Session::peer_max_auth_token_cache_size()` (peer SETUP の宣言値、未指定は 0) で取得する。
///
/// 期限切れ token の検出はアプリ責務である (Token Type 固有の期限判定はライブラリでは
/// 行えない。Type 0 は out-of-band 交渉)。draft-ietf-moq-transport-21 §8.9
/// (Authorization Token Compression): "If a receiver detects that an authorization token
/// has expired, it MUST retain the registered Alias until it is deleted by the sender"
/// のため、`resolve` は期限切れ後も alias を DELETE まで解決し続ける。アプリが期限切れを
/// 検出したときの応答コードは文脈ごとに異なる。
/// - 受信 request 文脈: `REQUEST_EXPIRED_AUTH_TOKEN` を `Session::send_request_error` に渡す
/// - SETUP などのセッション文脈: `SESSION_EXPIRED_AUTH_TOKEN` を `Session::close` に渡す
/// - data stream 文脈: `Session::reset_outgoing_data_stream` に
///   `DataStreamResetReason::ExpiredAuthToken` (コードは `STREAM_EXPIRED_AUTH_TOKEN`) を渡す
///
/// draft-ietf-moq-transport-21 §8.9 (Authorization Token Compression): "The receiver
/// of a message carrying an Authorization Token with Alias Type REGISTER that does not
/// result in a Session error MUST register the Token Alias in the token cache, even if
/// the message fails for other reasons" のため、request が別の理由で失敗する場合も
/// REGISTER は cache に登録する。期限切れを理由に拒否する場合も alias 登録は維持する。
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
    /// draft §8.9 (Authorization Token Compression): 未登録の alias を参照するとセッションエラー UNKNOWN_AUTH_TOKEN_ALIAS となるが、
    /// 本メソッドは単に `None` を返す。呼び出し側でエラーに変換する。
    pub fn resolve(&self, alias: u64) -> Option<(u64, &[u8])> {
        self.entries.get(&alias).map(|(t, v)| (*t, v.as_slice()))
    }

    /// DELETE: alias を除去する
    ///
    /// 未登録の alias を DELETE しようとした場合は単に no-op とする
    /// (draft §8.9 (Authorization Token Compression): 登録は sender 側の管理責任であり、unknown alias の DELETE を
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

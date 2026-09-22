//! Request ID 管理
//!
//! draft-ietf-moq-transport-21 §6.4.2.1 (Request ID) に基づく。
//! draft 由来の実装のため将来変更される可能性がある。

use alloc::collections::BTreeSet;

use crate::error::SESSION_INVALID_REQUEST_ID;

use super::types::{Role, SessionError};

/// Request ID 採番器 (自側)
///
/// draft-ietf-moq-transport-21 §6.4.2.1 (Request ID):
/// - Client は偶数 (0, 2, 4, ...) を採番
/// - Server は奇数 (1, 3, 5, ...) を採番
/// - 各エンドポイントは +2 ずつインクリメント
#[derive(Debug, Clone)]
pub struct RequestIdGenerator {
    next: u64,
}

impl RequestIdGenerator {
    /// 自側の役割に応じた開始値で作成する
    pub fn new(role: Role) -> Self {
        Self {
            next: match role {
                Role::Client => 0,
                Role::Server => 1,
            },
        }
    }

    /// 次の Request ID を発行する
    ///
    /// # Panics
    ///
    /// u64 オーバーフロー時 (偶数 / 奇数 を維持しながら u64::MAX まで使い切るのは非現実的)
    pub fn next_id(&mut self) -> u64 {
        let id = self.next;
        self.next = self
            .next
            .checked_add(2)
            .expect("Request ID generator overflowed u64");
        id
    }

    /// 次回発行予定の Request ID (peek)
    pub fn peek(&self) -> u64 {
        self.next
    }
}

/// 相手側の Request ID 追跡器
///
/// draft-ietf-moq-transport-21 §6.4.2.1 (Request ID):
/// - parity が送信者の role と合わない → INVALID_REQUEST_ID
/// - 重複した Request ID → INVALID_REQUEST_ID
///
/// 内部表現として「連続受信前縁 `front`」と「飛び飛びに受信したストラグラ集合 `above`」を保持する。
/// これにより peer が request を開閉し続けてもメモリは「同時に未到達な穴の上の受信済み id」数に
/// 比例し、累計受信総数には比例しない。
///
/// §6.4.2.1 (Request ID) が `INVALID_REQUEST_ID` でのセッション終了を MUST とするのは
/// parity 違反と重複の 2 条件だけであり、`above` の件数に上限を設けて超過時にセッションを
/// 閉じることは同節に無い。仕様に準拠した peer は Request ID を 2 ずつ連番で使うため前縁が
/// 埋まるたびに `above` から吸収され、保持数は同時に未到達な request 数で抑えられる。
/// 前縁が埋まらないまま飛び ID を送り続ける非準拠 peer では `above` が増え続けるが、
/// 上限超過で正当な peer を落とす方が interop への影響が大きいため、この増加は許容する。
#[derive(Debug, Clone)]
pub struct RequestIdTracker {
    peer_role: Role,
    /// peer の parity の base (Client=0, Server=1)
    parity_base: u64,
    /// base から連続して受信した最前縁の id (None = まだ 1 件も受信していない)
    /// 前縁が伸びるごとに above から吸収する
    front: Option<u64>,
    /// front を超えて飛び飛びに受信したストラグラの parity 正規化済み index (BTreeSet で順序保持)
    above: BTreeSet<u64>,
    /// 重複を除いた累計受理数 (seen_count 診断用)
    accepted_total: usize,
}

impl RequestIdTracker {
    /// 相手側の役割を指定して作成する
    pub fn new(peer_role: Role) -> Self {
        let parity_base = match peer_role {
            Role::Client => 0,
            Role::Server => 1,
        };
        Self {
            peer_role,
            parity_base,
            front: None,
            above: BTreeSet::new(),
            accepted_total: 0,
        }
    }

    /// id を parity 正規化済み index に変換する
    fn to_index(&self, id: u64) -> u64 {
        (id - self.parity_base) / 2
    }

    /// parity index を id に復元する
    fn to_id(&self, idx: u64) -> u64 {
        idx * 2 + self.parity_base
    }

    /// 受信した Request ID を検証・記録する
    ///
    /// draft §6.4.2.1 (Request ID) に基づき parity 検証と重複検出を行う。
    /// 違反時は [`SessionError`] を返す (いずれも INVALID_REQUEST_ID)。
    pub fn accept(&mut self, id: u64) -> Result<(), SessionError> {
        let expected_parity: u64 = match self.peer_role {
            Role::Client => 0,
            Role::Server => 1,
        };
        if id % 2 != expected_parity {
            return Err(SessionError::new(
                SESSION_INVALID_REQUEST_ID,
                "request id has wrong parity for sender",
            ));
        }

        let idx = self.to_index(id);

        // 重複判定: front 以下の idx は連続受信済みとみなす
        if let Some(front) = self.front
            && idx <= self.to_index(front)
        {
            return Err(SessionError::new(
                SESSION_INVALID_REQUEST_ID,
                "duplicate request id",
            ));
        }

        // above に含まれるか判定 (重複)
        if self.above.contains(&idx) {
            return Err(SessionError::new(
                SESSION_INVALID_REQUEST_ID,
                "duplicate request id",
            ));
        }

        // 初回受信
        self.accepted_total = self.accepted_total.saturating_add(1);

        // 新規受理した idx が front+1 なら front を前進させ、above から連続分を吸収する
        match self.front {
            None => {
                // 初回受信
                if idx == 0 {
                    // id == parity_base: front を確定
                    self.front = Some(id);
                    self.absorb_above();
                } else {
                    // 飛び飛びの初回: above に入れる
                    self.above.insert(idx);
                }
            }
            Some(front) => {
                let next_front_idx = self.to_index(front).saturating_add(1);
                if idx == next_front_idx {
                    // front の直後: 前進
                    self.front = Some(id);
                    self.absorb_above();
                } else {
                    // 飛び飛び: above に入れる
                    self.above.insert(idx);
                }
            }
        }

        Ok(())
    }

    /// front 前進後、above の先頭から連続する要素を front に吸収する
    fn absorb_above(&mut self) {
        let Some(front) = self.front else { return };
        let mut current_idx = self.to_index(front);
        loop {
            let next = current_idx.saturating_add(1);
            if self.above.remove(&next) {
                current_idx = next;
                self.front = Some(self.to_id(current_idx));
            } else {
                break;
            }
        }
    }

    /// 受信済み Request ID の数 (診断用)
    ///
    /// 重複を除いた累計受理数を返す (front/above の保持数とは異なる)。
    pub fn seen_count(&self) -> usize {
        self.accepted_total
    }
}

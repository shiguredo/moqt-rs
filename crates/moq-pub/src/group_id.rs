//! Group ID の採番
//!
//! draft-ietf-moq-msf-01 §6.1 (Group numbering) に基づき、配信開始時の Group ID を
//! Unix epoch ミリ秒から払い出す。publisher が再起動したときに以前 publish した
//! どの Group ID よりも大きい値から始めなければならない (MUST) ため、同一プロセス内でも
//! 前回の割当てを必ず上回る値にする。

use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use crate::error::Error;
use crate::error::Result;

/// 同一プロセス内で最後に払い出した開始 Group ID
///
/// 映像 / 音声 / カタログの 3 トラックで共有する。track をまたいで大きめに倒すのは
/// 安全側であり、ある track の割当てが別の track の単調増加ガードを押し上げても
/// 仕様上の問題はない。
static LAST_INITIAL_GROUP_ID: AtomicU64 = AtomicU64::new(0);

/// 配信開始時の Group ID を払い出す
///
/// draft-ietf-moq-msf-01 §6.1 (Group numbering):
/// "Group IDs for a track MUST be unique and MUST increase monotonically." /
/// "When a publisher restarts (e.g., after connectivity loss or encoder restart), it MUST
/// ensure the new starting Group ID is greater than any previously published Group ID for
/// that track. One approach is to use the current wall clock time in milliseconds since the
/// Unix epoch as the starting Group ID."
///
/// 壁時計が巻き戻っても、同一プロセス内では前回の割当てを上回る値を返す。
///
/// # Errors
///
/// システム時刻が Unix epoch より前、または u64 のミリ秒で表現できない場合はエラーになる。
pub fn allocate_initial_group_id() -> Result<u64> {
    let candidate = wall_clock_ms()?;
    Ok(next_initial_group_id(candidate, &LAST_INITIAL_GROUP_ID))
}

/// 候補値と前回値から次の開始 Group ID を決める (単調増加ガード)
///
/// 候補値が前回値以下なら前回 + 1 を返す。`u64::MAX` まで使い切った場合は飽和させ、
/// 同じ値を返す (送れない値を作らない)。
fn next_initial_group_id(candidate: u64, last: &AtomicU64) -> u64 {
    let mut previous = last.load(Ordering::Relaxed);
    loop {
        let next = candidate.max(previous.saturating_add(1));
        match last.compare_exchange_weak(previous, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => return next,
            Err(observed) => previous = observed,
        }
    }
}

/// カタログを送り直すときの次の Group ID
///
/// draft-ietf-moq-msf-01 §6.1 (Group numbering): "Within a continuous publishing session,
/// each subsequent Group ID SHOULD increase by 1."。また、独立したカタログは新しい Group の
/// 先頭に置かなければならない (§5 (Catalog))。
pub fn next_group_id(current: u64) -> u64 {
    current.saturating_add(1)
}

/// 現在時刻を Unix epoch からのミリ秒で返す
fn wall_clock_ms() -> Result<u64> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(format!("system clock is before the Unix epoch: {e}")))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| Error::Other("system clock is out of the supported range".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 候補値が前回値を上回るときは候補値をそのまま使うこと
    ///
    /// 通常の起動 (壁時計が単調に進む) では Unix epoch ミリ秒がそのまま開始 Group ID になる。
    #[test]
    fn next_initial_group_id_uses_the_candidate_when_it_is_greater() {
        let last = AtomicU64::new(1_000);
        assert_eq!(
            next_initial_group_id(2_000, &last),
            2_000,
            "候補値が前回値を上回るときは候補値を使うこと"
        );
        assert_eq!(
            last.load(Ordering::Relaxed),
            2_000,
            "払い出した値が次のガードになること"
        );
    }

    /// 候補値が前回値以下のときは前回 + 1 を返すこと
    ///
    /// 短時間の停止と再開、または壁時計の巻き戻しで、再開時の開始 Group ID が前回の
    /// 割当てを下回らないようにする (draft-ietf-moq-msf-01 §6.1 の MUST)。
    #[test]
    fn next_initial_group_id_stays_above_the_previous_value() {
        let last = AtomicU64::new(1_000);
        assert_eq!(
            next_initial_group_id(500, &last),
            1_001,
            "候補値が前回値以下なら前回 + 1 を返すこと"
        );
        assert_eq!(
            next_initial_group_id(500, &last),
            1_002,
            "同じ候補値で繰り返し呼んでも単調増加すること"
        );
        assert_eq!(
            next_initial_group_id(1_002, &last),
            1_003,
            "候補値が前回値と等しいときも前回 + 1 を返すこと (Location を重複させない)"
        );
    }

    /// u64 の上限では飽和し、同じ値を返すこと
    #[test]
    fn next_initial_group_id_saturates_at_the_maximum() {
        let last = AtomicU64::new(u64::MAX);
        assert_eq!(
            next_initial_group_id(u64::MAX, &last),
            u64::MAX,
            "上限では飽和させること"
        );
    }

    /// カタログの送り直しは Group ID を 1 進めること
    ///
    /// draft-ietf-moq-msf-01 §6.1 (Group numbering) は連続した配信で次の Group ID を 1 進める
    /// ことを SHOULD とする。同じ Location を 2 度送ると購読側で重複として扱われるため、
    /// 送り直しでは必ず進める。
    #[test]
    fn next_group_id_increases_by_one() {
        assert_eq!(next_group_id(0), 1);
        assert_eq!(next_group_id(1_760_000_000_000), 1_760_000_000_001);
        assert_eq!(next_group_id(u64::MAX), u64::MAX, "上限では飽和させること");
    }

    /// 現在時刻が Unix epoch ミリ秒として取得できること
    #[test]
    fn wall_clock_ms_is_positive() {
        let now = wall_clock_ms().expect("現在時刻を取得できること");
        assert!(now > 0, "Unix epoch ミリ秒は 0 より大きいこと: {now}");
    }
}

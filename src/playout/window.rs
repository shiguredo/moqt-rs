//! 時刻付き値のローリング窓
//!
//! playout と audio_clock の各所にあった「時刻と値の組を古い順に保持し、古いものを
//! 先頭から捨てる」処理を 1 箇所に集約する。moqt-js の Lean 検証
//! (`PlayoutAudit/Window.lean`) にある `prefix_prune_equivalent` と同じく、記録が時刻順で
//! ある限り、先頭からの切り捨ては全件を age で filter した結果と一致する。
//!
//! 境界の扱いは呼び出し側の用途で異なるため、メソッド名で明示する。
//! - [`TimedWindow::prune`] は `oldest_us` を含む (それより前だけを捨てる)
//! - [`TimedWindow::prune_excluding`] は `oldest_us` を含まない (それ以下を捨てる)
//! - [`TimedWindow::min_after`] は `since_us` を含む
//!
//! 件数の上限は置かない。窓の長さと観測頻度で件数が決まる既存の挙動を保つ。

use alloc::collections::VecDeque;
use alloc::vec::Vec;

/// 時刻付き値のローリング窓 (古い順)
///
/// 記録の時刻は単調に増える前提である。`at_us` が前の記録以下になる入力は
/// 呼び出し側の責務で避ける (窓は並べ替えない)。
#[derive(Debug, Default)]
pub(crate) struct TimedWindow {
    entries: VecDeque<(i64, i64)>,
}

impl TimedWindow {
    /// 観測を 1 つ足す
    pub(crate) fn push(&mut self, at_us: i64, value_us: i64) {
        self.entries.push_back((at_us, value_us));
    }

    /// `oldest_us` より前の観測を捨てる (境界を含む)
    ///
    /// `oldest_us` ちょうどの観測は残す。
    pub(crate) fn prune(&mut self, oldest_us: i64) {
        while self
            .entries
            .front()
            .is_some_and(|(at_us, _)| *at_us < oldest_us)
        {
            self.entries.pop_front();
        }
    }

    /// `oldest_us` 以下の観測を捨てる (境界を含まない)
    ///
    /// 音声の受信側 catch_up のように「この時刻より後の観測だけ」を使う用途向けである。
    pub(crate) fn prune_excluding(&mut self, oldest_us: i64) {
        while self
            .entries
            .front()
            .is_some_and(|(at_us, _)| *at_us <= oldest_us)
        {
            self.entries.pop_front();
        }
    }

    /// 窓の中の最小値
    pub(crate) fn min_value(&self) -> Option<i64> {
        self.entries.iter().map(|(_, value_us)| *value_us).min()
    }

    /// 窓の中の最も古い記録 (時刻, 値)。無ければ None
    pub(crate) fn oldest(&self) -> Option<(i64, i64)> {
        self.entries.front().copied()
    }

    /// 窓の中の最も新しい記録 (時刻, 値)。無ければ None
    pub(crate) fn latest(&self) -> Option<(i64, i64)> {
        self.entries.back().copied()
    }

    /// 窓の中の記録を古い順に返す
    pub(crate) fn iter(&self) -> impl DoubleEndedIterator<Item = (i64, i64)> + '_ {
        self.entries.iter().copied()
    }

    /// `since_us` 以降の記録の最小値。無ければ None (境界を含む)
    ///
    /// 窓より短い区間の最小値を取り直すために使う。窓全体の最小値は、値が単調に動いて
    /// いるときに最も古い観測を指したままになる。
    pub(crate) fn min_after(&self, since_us: i64) -> Option<i64> {
        let mut min: Option<i64> = None;
        for (at_us, value_us) in self.entries.iter().rev() {
            if *at_us < since_us {
                break;
            }
            min = Some(min.map_or(*value_us, |min| min.min(*value_us)));
        }
        min
    }

    /// 昇順に並べた、窓の中の値
    pub(crate) fn sorted_values(&self) -> Vec<i64> {
        let mut values: Vec<i64> = self.values_vec();
        values.sort_unstable();
        values
    }

    /// 窓の中の値 (古い順)
    pub(crate) fn values_vec(&self) -> Vec<i64> {
        self.entries.iter().map(|(_, value_us)| *value_us).collect()
    }

    /// 記録をすべて捨てる
    pub(crate) fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    /// 窓の境界を含む prune は、同じ時刻の記録を残すこと
    #[test]
    fn prune_keeps_boundary() {
        let mut window = TimedWindow::default();
        window.push(10, 1);
        window.push(20, 2);
        window.push(30, 3);
        window.prune(20);
        assert_eq!(window.oldest(), Some((20, 2)));
        assert_eq!(window.values_vec(), vec![2, 3]);
    }

    /// 窓の境界を含まない prune は、同じ時刻の記録を捨てること
    #[test]
    fn prune_excluding_drops_boundary() {
        let mut window = TimedWindow::default();
        window.push(10, 1);
        window.push(20, 2);
        window.push(30, 3);
        window.prune_excluding(20);
        assert_eq!(window.oldest(), Some((30, 3)));
        assert_eq!(window.values_vec(), vec![3]);
    }

    /// min_after は境界を含むこと
    #[test]
    fn min_after_includes_boundary() {
        let mut window = TimedWindow::default();
        window.push(10, 5);
        window.push(20, 3);
        window.push(30, 7);
        assert_eq!(window.min_after(20), Some(3));
        assert_eq!(window.min_after(25), Some(7));
        assert_eq!(window.min_after(31), None);
    }

    /// 空の窓は最小値も最古の記録も返さないこと
    #[test]
    fn empty_window_has_no_values() {
        let window = TimedWindow::default();
        assert_eq!(window.min_value(), None);
        assert_eq!(window.oldest(), None);
        assert_eq!(window.min_after(0), None);
        assert!(window.sorted_values().is_empty());
    }
}

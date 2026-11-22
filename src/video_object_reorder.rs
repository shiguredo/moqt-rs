//! object モードで受信した映像 Object を Group ごと / Object ID 順に並べ替えるバッファ
//!
//! draft-ietf-moq-msf-02 §4.1.1 (LOC to MOQT Stream mapping) の `object` モードでは、
//! publisher は 1 Object を 1 Subgroup に載せ、Object ごとに別の data stream で送る。
//! draft-ietf-moq-transport-23 §2.2 (Subgroups) が順序を保証するのは stream 内だけである
//! ため、受信側には複数の Subgroup が到着順に入れ替わって届く。
//!
//! §6.2 (Object Numbering) は Group 内の最初の Object ID を 0 とし、以降 1 ずつ増やすことを
//! MUST とする。本バッファはこの規則を使い、Group ごとに Object ID の連続した範囲だけを
//! 取り出す。届いていない Object ID がある間は後続を保留し、到着順の入れ替わりを吸収する。
//!
//! - 新しい Group を受信したら、それより古い Group の保留分は Object ID 順に取り出す。
//!   Group の先頭 (Object ID 0) を受信しないまま次の Group へ進んだ場合 (購読を Group の
//!   途中から始めた場合など) も、保留したままにせず取り出して復号順の判定へ渡す。
//! - 同じ Group のうち、配送を始めた Object ID より小さい Object と、同じ Object ID の
//!   重複は捨てる。
//! - 保留量には上限を設け、超えたら最も古い Group の保留分を Object ID 順に取り出す。
//!   Group の先頭が永久に届かない購読でもメモリ使用量を抑えるためである。
//!
//! 判定に使うのは Group ID と Object ID だけで、時刻も I/O も扱わない (sans I/O)。
//! 保持する値の型 `T` は呼び出し側が決める。
//!
//! 根拠の仕様はドラフトであり、将来変更される可能性がある。

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

/// 保留する Object 数の上限
///
/// Group 長は 1 GOP であり、映像のキーフレーム間隔が数秒なら Object 数は数百に収まる。
/// 購読を Group の途中から始めた場合に次の Group まで保留し続けても、この上限で
/// メモリ使用量を抑える。
const MAX_PENDING_OBJECTS: usize = 1024;

/// 並べ替えた結果として取り出した Object
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReorderedVideoObject<T> {
    /// Object の Group ID
    pub group_id: u64,
    /// Group 内の Object ID
    pub object_id: u64,
    /// 呼び出し側が保持していた値 (payload など)
    pub value: T,
}

/// 1 Group 分の保留状態
#[derive(Debug)]
struct GroupBuffer<T> {
    /// 次に取り出す Object ID
    ///
    /// §6.2 により Group の最初の Object ID は 0 である。
    next_object_id: u64,
    /// Object ID が `u64::MAX` の Object まで取り出し終えたか
    ///
    /// `u64::MAX + 1` は表現できないため、`next_object_id` を進める代わりにこの
    /// フラグを立てる。以後この Group の Object はすべて重複または遅着として捨てる。
    exhausted: bool,
    /// 受信済みで、まだ取り出していない Object (Object ID 順)
    pending: BTreeMap<u64, T>,
    /// この Group より新しい Group を受信したため、欠けを待たずに取り出すかどうか
    flushing: bool,
}

/// Object モードの映像 Object を Group ごと / Object ID 順に並べ替えるバッファ
///
/// 購読 (復号経路) ごとに 1 つ持つ。複数の stream task から受信した Object を
/// [`VideoObjectReorderBuffer::insert`] へ渡し、戻り値の順に復号経路へ流す。
#[derive(Debug)]
pub struct VideoObjectReorderBuffer<T> {
    /// Group ID 順の保留状態。先頭の Group から取り出す
    groups: BTreeMap<u64, GroupBuffer<T>>,
    /// 受信した最新の Group ID
    newest_group_id: Option<u64>,
    /// 最後に取り出した Object の Group ID
    ///
    /// これより古い Group の Object は、既に次の Group を配送した後なので捨てる。
    last_released_group_id: Option<u64>,
}

impl<T> VideoObjectReorderBuffer<T> {
    /// 保留の無い状態で作る
    pub fn new() -> Self {
        Self {
            groups: BTreeMap::new(),
            newest_group_id: None,
            last_released_group_id: None,
        }
    }

    /// 受信した Object を追加し、配送してよい Object を Object ID 順に返す
    ///
    /// 戻り値は配送してよい順に並んでいる。呼び出し側はこの順に復号経路へ流す。
    /// 到着順が入れ替わっていても、Object ID の連続した範囲が揃うまで保留する。
    pub fn insert(
        &mut self,
        group_id: u64,
        object_id: u64,
        value: T,
    ) -> Vec<ReorderedVideoObject<T>> {
        let mut ready = Vec::new();
        // 既に後ろの Group を配送した後なら、この Object は古い (次の Group のキーフレームを
        // 復号した後に前の Group の delta を復号すると参照が壊れる)
        if self
            .last_released_group_id
            .is_some_and(|last| group_id < last)
        {
            return ready;
        }

        // 新しい Group を受信したら、それより古い Group の保留分はもう揃わないため、
        // Object ID 順に取り出して次の Group の配送を妨げないようにする
        if self.newest_group_id.is_none_or(|newest| group_id > newest) {
            self.newest_group_id = Some(group_id);
            for (_, group) in self.groups.range_mut(..group_id) {
                group.flushing = true;
            }
        }

        let group = self.groups.entry(group_id).or_insert_with(|| GroupBuffer {
            // §6.2 (Object Numbering): Group 内の最初の Object ID は 0
            next_object_id: 0,
            exhausted: false,
            pending: BTreeMap::new(),
            flushing: false,
        });
        // 既に取り出した Object ID は重複か遅着であり、同じ Object ID の再受信は最初の
        // 1 つを使う
        if !group.exhausted && object_id >= group.next_object_id {
            group.pending.entry(object_id).or_insert(value);
        }

        self.drain(&mut ready);
        // 保留量が上限を超えたら、最も古い Group の保留分を Object ID 順に取り出す。
        // Group の先頭が永久に届かない購読でもメモリ使用量を抑えるための保険である
        while self.pending_len() > MAX_PENDING_OBJECTS {
            let Some(&oldest) = self
                .groups
                .iter()
                .find(|(_, g)| !g.pending.is_empty())
                .map(|(id, _)| id)
            else {
                break;
            };
            if let Some(group) = self.groups.get_mut(&oldest) {
                group.flushing = true;
            }
            self.drain(&mut ready);
        }
        ready
    }

    /// 保留をすべて捨てて初期状態に戻す
    ///
    /// session 移行などで購読の状態を作り直すときに呼ぶ。旧 session の Object を配送すると、
    /// 新しい session の復号順の判定を古い位置で汚すためである。
    pub fn reset(&mut self) {
        self.groups.clear();
        self.newest_group_id = None;
        self.last_released_group_id = None;
    }

    /// 保留している Object 数
    fn pending_len(&self) -> usize {
        self.groups.values().map(|group| group.pending.len()).sum()
    }

    /// 配送してよい Object を取り出す
    ///
    /// Group ID の昇順に処理する。新しい Group を受信済みなら、それより古い Group は
    /// `flushing` であり、欠けを待たずにすべて取り出す。
    fn drain(&mut self, ready: &mut Vec<ReorderedVideoObject<T>>) {
        let group_ids: Vec<u64> = self.groups.keys().copied().collect();
        for group_id in group_ids {
            let Some(group) = self.groups.get_mut(&group_id) else {
                continue;
            };
            if group.flushing {
                // この Group の後に別の Group が受信済みであり、欠けはもう揃わない。
                // 受信済みの Object を Object ID 順に取り出す
                let pending = core::mem::take(&mut group.pending);
                for (object_id, value) in pending {
                    ready.push(ReorderedVideoObject {
                        group_id,
                        object_id,
                        value,
                    });
                    self.last_released_group_id = Some(group_id);
                    // §6.2 (Object Numbering): 次の Object ID は u64 の範囲を超えない。
                    // u64::MAX を取り出したら、この Group で取り出せる Object はもう無い。
                    match object_id.checked_add(1) {
                        Some(next_object_id) => group.next_object_id = next_object_id,
                        None => {
                            group.exhausted = true;
                            break;
                        }
                    }
                }
                continue;
            }
            while let Some(value) = group.pending.remove(&group.next_object_id) {
                let object_id = group.next_object_id;
                ready.push(ReorderedVideoObject {
                    group_id,
                    object_id,
                    value,
                });
                self.last_released_group_id = Some(group_id);
                match object_id.checked_add(1) {
                    Some(next_object_id) => group.next_object_id = next_object_id,
                    None => {
                        group.exhausted = true;
                        break;
                    }
                }
            }
        }
        // 配送を終えた古い Group の状態は、遅着 Object を捨てる判定に不要になったら捨てる。
        // 最新の Group は、同じ Group の後続 Object を続けて取り出すために残す
        let newest_group_id = self.newest_group_id;
        self.groups.retain(|group_id, group| {
            !group.pending.is_empty() || Some(*group_id) == newest_group_id
        });
    }
}

impl<T> Default for VideoObjectReorderBuffer<T> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    /// 取り出した Object の (Group ID, Object ID) を返す
    fn locations<T>(objects: &[ReorderedVideoObject<T>]) -> Vec<(u64, u64)> {
        objects
            .iter()
            .map(|object| (object.group_id, object.object_id))
            .collect()
    }

    /// 到着順が Object ID 順なら、受信した順に取り出されること
    #[test]
    fn releases_objects_in_order() {
        let mut buffer = VideoObjectReorderBuffer::new();
        assert_eq!(locations(&buffer.insert(10, 0, "a")), vec![(10, 0)]);
        assert_eq!(locations(&buffer.insert(10, 1, "b")), vec![(10, 1)]);
        assert_eq!(locations(&buffer.insert(10, 2, "c")), vec![(10, 2)]);
    }

    /// 先に後ろの Object が届いても、先頭が揃った時点で Object ID 順に取り出されること
    #[test]
    fn reorders_objects_within_a_group() {
        let mut buffer = VideoObjectReorderBuffer::new();
        assert!(
            buffer.insert(10, 1, "b").is_empty(),
            "Object 0 が届くまでは保留すること"
        );
        assert_eq!(
            locations(&buffer.insert(10, 0, "a")),
            vec![(10, 0), (10, 1)],
            "先頭が揃ったら保留分を Object ID 順に取り出すこと"
        );
        // 2 より先に 3 が届いても、2 が揃えば 2 と 3 を続けて取り出す
        assert!(
            buffer.insert(10, 3, "d").is_empty(),
            "Object 2 が届くまでは Object 3 も保留すること"
        );
        assert_eq!(
            locations(&buffer.insert(10, 2, "c")),
            vec![(10, 2), (10, 3)]
        );
    }

    /// 新しい Group を受信したら、前の Group の保留分を Object ID 順に取り出すこと
    ///
    /// Group の先頭を受信しないまま次に進んだ場合 (購読を Group の途中から始めた場合) も、
    /// 保留したままにしない。
    #[test]
    fn flushes_older_groups_when_a_newer_group_arrives() {
        let mut buffer = VideoObjectReorderBuffer::new();
        assert!(
            buffer.insert(10, 2, "a").is_empty(),
            "Group 10 の先頭が届くまでは保留すること"
        );
        assert_eq!(
            locations(&buffer.insert(11, 0, "b")),
            vec![(10, 2), (11, 0)],
            "新しい Group の受信で古い Group の保留分を先に取り出すこと"
        );
        assert_eq!(locations(&buffer.insert(11, 1, "c")), vec![(11, 1)]);
    }

    /// 既に取り出した Object の重複と遅着を捨てること
    #[test]
    fn drops_duplicate_and_stale_objects() {
        let mut buffer = VideoObjectReorderBuffer::new();
        assert_eq!(locations(&buffer.insert(10, 0, "a")), vec![(10, 0)]);
        assert!(
            buffer.insert(10, 0, "duplicate").is_empty(),
            "同じ Object ID の再受信は取り出さないこと"
        );
        assert!(
            buffer.insert(10, 0, "stale").is_empty(),
            "既に取り出した Object ID は捨てること"
        );
        // 次の Group を配送した後に前の Group の Object が届いても捨てる
        assert_eq!(locations(&buffer.insert(11, 0, "b")), vec![(11, 0)]);
        assert!(
            buffer.insert(10, 1, "late").is_empty(),
            "後ろの Group を配送した後に届いた古い Group の Object は捨てること"
        );
    }

    /// 同じ Group の Object ID が飛んでいても、次の Group で打ち切られること
    #[test]
    fn missing_object_is_skipped_when_the_next_group_arrives() {
        let mut buffer = VideoObjectReorderBuffer::new();
        assert_eq!(locations(&buffer.insert(10, 0, "a")), vec![(10, 0)]);
        assert!(
            buffer.insert(10, 2, "c").is_empty(),
            "Object 1 の到着を待つこと"
        );
        assert_eq!(
            locations(&buffer.insert(11, 0, "b")),
            vec![(10, 2), (11, 0)],
            "欠けた Object は待たずに次の Group へ進むこと"
        );
    }

    /// 保留量が上限を超えたら最も古い Group の保留分を取り出すこと
    ///
    /// Group の先頭が永久に届かない購読でメモリ使用量が増え続けないことを確かめる。
    #[test]
    fn releases_the_oldest_group_when_the_pending_limit_is_exceeded() {
        let mut buffer = VideoObjectReorderBuffer::new();
        // Group 10 の先頭 (Object 0) は届かないまま、上限を超える数の Object を受信する
        let mut released = Vec::new();
        for object_id in 1..=(MAX_PENDING_OBJECTS as u64 + 1) {
            released.extend(buffer.insert(10, object_id, object_id));
        }
        assert!(!released.is_empty(), "上限を超えたら保留分を取り出すこと");
        assert_eq!(
            released.first().map(|object| object.object_id),
            Some(1),
            "取り出した Object は Object ID 順であること"
        );
        assert_eq!(
            released.last().map(|object| object.object_id),
            Some(MAX_PENDING_OBJECTS as u64 + 1),
            "上限を超えた Object まで取り出すこと"
        );
    }

    /// `u64::MAX` の Object を取り出した後、同じ Group の Object ID 0 を再受信しても
    /// 重複として捨てること
    ///
    /// `next_object_id` を通常の `+ 1` で進めると `u64::MAX + 1` が 0 に折り返し、
    /// 取り出し済みの Object 0 を受け付けてしまう。Object ID は u64 全域を表現できるため、
    /// 上限では「これ以上取り出さない」状態へ遷移する必要がある。
    #[test]
    fn object_id_u64_max_is_terminal() {
        let mut buffer = VideoObjectReorderBuffer::new();
        // Object 0 は即座に取り出し、1 は欠けたまま 2..=1025 を保留して上限超過を作る
        assert_eq!(locations(&buffer.insert(10, 0, 0u64)), vec![(10, 0)]);
        let mut released = Vec::new();
        for object_id in 2..=(MAX_PENDING_OBJECTS as u64 + 1) {
            released.extend(buffer.insert(10, object_id, object_id));
        }
        assert!(released.is_empty(), "上限ちょうどではまだ取り出さないこと");

        // u64::MAX を追加すると上限を超え、保留分を Object ID 順に取り出す。
        // 最後に u64::MAX が来るため、この Group は「取り出し済みの終端」になる。
        released.extend(buffer.insert(10, u64::MAX, u64::MAX));
        assert_eq!(
            released.last().map(|object| object.object_id),
            Some(u64::MAX),
            "u64::MAX まで取り出すこと"
        );

        // 折り返した 0 を受け付けると重複配送になる。終端後は捨てること。
        assert!(
            buffer.insert(10, 0, 999).is_empty(),
            "u64::MAX の後に Object ID 0 を再受信しても重複として捨てること"
        );
        assert!(
            buffer.insert(10, u64::MAX, 999).is_empty(),
            "u64::MAX の再受信も重複として捨てること"
        );
    }

    /// `reset` で保留と Group の状態が消えること
    #[test]
    fn reset_discards_pending_objects() {
        let mut buffer = VideoObjectReorderBuffer::new();
        assert!(buffer.insert(10, 1, "a").is_empty());
        buffer.reset();
        assert_eq!(
            locations(&buffer.insert(10, 0, "b")),
            vec![(10, 0)],
            "reset 後は最初から Object ID 順に取り出すこと"
        );
    }
}

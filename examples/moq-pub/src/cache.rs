//! publish 済みメディアの保持と FETCH への応答の判断
//!
//! この example が publish した映像 / 音声の Object を `--cache-duration` の時間窓だけ
//! Group 単位で保持し、subscriber からの FETCH (draft-ietf-moq-transport-22 §3.2 (Fetch) /
//! §9.11 (FETCH)) に配る Object と End of Unknown Range の位置を決める。
//!
//! 期限切れの判定は Object を保持した時刻 (publisher の壁時計、[`Instant`]) で行う。Group ID は
//! 配信開始時に Unix epoch ミリ秒から払い出し、以降は 1 ずつ進むため
//! (draft-ietf-moq-msf-01 §6.1 (Group numbering))、Group ID から経過時間は復元できない。
//! 保持量の上限は時間窓だけで決める。Group 数やバイト数の上限を別に設けると、トラックごとに
//! 実際に保持できる時間が変わり (音声は 1 Object = 1 Group で 20 ms 間隔)、
//! draft-ietf-moq-transport-22 §10.3 (MAX CACHE DURATION) が示す「保持から配れる時間」と
//! 一致しなくなる。
//!
//! 保持するのは payload と LOC Properties の両方である。FETCH 応答の Object は Properties
//! フィールドを持つため、Properties は Properties Length varint を含む生バイト列として保持する
//! ([`LocProperties::encode`] の結果をそのまま持つ。空でも長さ 0 を含む。
//! draft-ietf-moq-transport-22 §11.1.2 (Object Properties))。

use std::collections::BTreeMap;
use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::message::common::Location;
use shiguredo_moqt::message::common::TrackNamespace;

use crate::error::Result;

/// FETCH の GROUP_ORDER が Descending である値 (draft-ietf-moq-transport-22 §9.20.8 (GROUP ORDER Parameter))
const GROUP_ORDER_DESCENDING: u8 = 0x02;

/// 保持している 1 件の Object
#[derive(Debug)]
pub struct CachedObject {
    /// この Object の Location
    pub location: Location,
    /// Properties Length varint を含む LOC Properties の生バイト列
    pub properties_data: Vec<u8>,
    /// Object のペイロード
    pub payload: Vec<u8>,
}

/// FETCH への応答内容
///
/// 判定の順序は「対象外の track」→「要求の GROUP_ORDER」→「保持内容」である。Session が
/// 受理した FETCH だけが呼び出し側へ届くため、受理の判定 (publish 実績の有無、空 range、
/// Start Location が Largest Object を上回るか) はここでやり直さない。やり直すと、拒否済みの
/// request に応答して致命終了するか、受理済みの request に無応答になる。
#[derive(Debug)]
pub enum FetchPlan<'a> {
    /// この example が PUBLISH していない track (保持している Object も無い)
    ///
    /// 呼び出し側が `REQUEST_DOES_NOT_EXIST` で拒否する。Session は publish 実績の無い track の
    /// FETCH を受理するため、応答が必要になる。
    UnknownTrack,
    /// PUBLISH しているが、まだ 1 件も Object を publish していない
    ///
    /// 呼び出し側が `REQUEST_INVALID_RANGE` で拒否する (draft-ietf-moq-transport-22 §3.2 は
    /// 「Object が publish されていない」場合に INVALID_RANGE を返す MUST を課す)。
    /// Session が自側 subscription の Largest Object を観測していれば Session が先に拒否する
    /// ため、通常この分岐には到達しない (防御)。
    NoPublishedObjects,
    /// 要求された GROUP_ORDER では応答しない
    ///
    /// 呼び出し側が `REQUEST_NOT_SUPPORTED` で拒否する。draft-ietf-moq-transport-22 §3.2.1 は
    /// 要求された group order で送る MUST を課すため、昇順の保持範囲を返さない。
    UnsupportedGroupOrder,
    /// 要求 range の Object を 1 つも保持しておらず、その範囲が evict 済みである
    ///
    /// FETCH_OK の End Location に置く End of Unknown Range を 1 件だけ書く。evict 済みは
    /// 「存在しない」ではなく「不明」である (draft-ietf-moq-transport-22 §2.1.2 (Object States) / §10.3)。
    AllUnknown {
        /// End of Unknown Range の Location (= FETCH_OK の End Location)
        end_location: Location,
    },
    /// 要求 range に、一度も publish していない範囲しか無い
    ///
    /// Object が 1 つも存在しないため、FETCH_OK を返して「FETCH_HEADER だけの stream」を
    /// FIN で閉じる (draft-ietf-moq-transport-22 §3.2.1 (Fetch Object Delivery))。
    /// 「不明」ではないため End of Range を書かない (§3.2.2 は未マークのギャップを
    /// 「存在しない」ものとして扱うと定める)。
    EmptyRange,
    /// 保持している Object を Location の昇順で返す
    Serve {
        /// 最初の Object より前に置く End of Non-Existent Range
        ///
        /// 要求 Start が「最初に publish した Object」より前のときだけ `Some` になる。
        /// そのままでは §3.2.2 の既定 (未マークのギャップは存在しない) で正しく表せるが、
        /// 続けて End of Unknown Range を置くと、その区間が存在しない範囲まで「不明」に
        /// 含まれてしまうため、境界を明示して分割する (§3.2.2 が認める唯一の用途)。
        leading_non_existent: Option<Location>,
        /// 最初の Object より前に置く End of Unknown Range
        ///
        /// 要求 Start 側が evict 済みのときだけ `Some` になる。draft-ietf-moq-transport-22
        /// §11.4.1.2 (End of Range) の End of Unknown Range は「直前に書いた entry の Location
        /// から自分の Location までの区間」を表す位置依存の表記であり、最初の entry に置くと
        /// 要求 range の先頭から自分の Location までが不明であることを表す。
        leading_unknown: Option<Location>,
        /// 返す Object (要求 range 内、Location の昇順)
        objects: Vec<&'a CachedObject>,
    },
}

/// 1 つの Group の保持内容
#[derive(Debug)]
struct GroupCache {
    /// この Group の Object を最後に保持した時刻
    ///
    /// Group 単位で evict するため、Group の保持期限はここから測る。Object ごとに
    /// evict すると保持範囲の内部に欠落ができ、End of Unknown Range を区間の途中に
    /// 置かなければならなくなる (時間窓で evict されるのは古い側だけである)。
    retained_at: Instant,
    /// Object ID ごとの保持 Object (Object ID の昇順)
    objects: BTreeMap<u64, CachedObject>,
}

/// 1 つの track の保持内容
#[derive(Debug, Default)]
struct TrackCache {
    /// Group ID ごとの保持内容 (Group ID の昇順)
    groups: BTreeMap<u64, GroupCache>,
    /// publish 済みの Largest Object
    ///
    /// evict 後も保持する。FETCH の End Location の決定と、要求 range 全体が evict 済みの
    /// 場合の判断に使う。
    largest: Option<Location>,
    /// 最初に publish した Object の Location
    ///
    /// これより小さい Location は一度も publish していない (「存在しない」)。
    /// End of Unknown Range が「存在しない」範囲まで「不明」と主張しないための境界である。
    first_published: Option<Location>,
    /// evict した Object のうち最後の (最も大きい) Location
    ///
    /// FETCH 応答で End of Unknown Range を置く位置を決めるために使う。evict する Location は
    /// 単調増加するため、最大値だけを保持すれば「要求 range 内に evict 済みの Object があるか」
    /// を判定できる。
    last_evicted: Option<Location>,
}

/// FETCH 応答に書く entry
///
/// 1 本の uni stream に書く順序そのものである。End of Range は「直前に書いた entry の
/// Location から自分の Location までの区間」を表す位置依存の表記であるため
/// (draft-ietf-moq-transport-22 §11.4.1.2)、順序を純関数として固定する。
#[derive(Debug)]
pub enum FetchResponseEntry<'a> {
    /// End of Non-Existent Range (存在しない範囲)
    NonExistent(Location),
    /// End of Unknown Range (不明な範囲)
    Unknown(Location),
    /// 保持している Object
    Object(&'a CachedObject),
}

impl FetchPlan<'_> {
    /// 応答に書く entry の並びを返す
    ///
    /// `UnknownTrack` / `NoPublishedObjects` / `UnsupportedGroupOrder` は応答を書かないため
    /// 空を返す。`EmptyRange` は FETCH_HEADER だけを書くため空を返す (§3.2.1)。
    pub fn entries(&self) -> Vec<FetchResponseEntry<'_>> {
        match self {
            FetchPlan::Serve {
                leading_non_existent,
                leading_unknown,
                objects,
            } => {
                let mut entries = Vec::new();
                if let Some(location) = leading_non_existent {
                    entries.push(FetchResponseEntry::NonExistent(*location));
                }
                if let Some(location) = leading_unknown {
                    entries.push(FetchResponseEntry::Unknown(*location));
                }
                entries.extend(objects.iter().copied().map(FetchResponseEntry::Object));
                entries
            }
            FetchPlan::AllUnknown { end_location } => {
                vec![FetchResponseEntry::Unknown(*end_location)]
            }
            _ => Vec::new(),
        }
    }
}

/// publish 済みメディアの保持
///
/// track ごとに Object を Group 単位で保持し、`--cache-duration` の時間窓を過ぎた Group を
/// evict する。期限切れの評価は保持への追加時と FETCH 応答の判断時の双方で行う
/// ([`MediaCache::insert`] と [`MediaCache::plan`])。
pub struct MediaCache {
    /// 保持する時間窓 (ミリ秒)
    duration_ms: u64,
    /// namespace ごとの track の保持内容
    ///
    /// namespace で分けてから track 名で引く。参照 (`&TrackNamespace` / `&[u8]`) で
    /// 引けるようにするため、`(TrackNamespace, Vec<u8>)` を 1 つのキーにはしない。
    tracks: HashMap<TrackNamespace, HashMap<Vec<u8>, TrackCache>>,
}

impl MediaCache {
    /// 時間窓 `duration_ms` (ミリ秒) の保持を作る
    ///
    /// 0 は「保持しない」を意味する。publish した Object は配信のたびに保持されるが、次の
    /// Object を保持する時点で期限切れになり、FETCH には常に End of Unknown Range を返す。
    /// Largest Object は evict 後も残るため、FETCH 自体は受理されて応答できる。
    pub fn new(duration_ms: u64) -> Self {
        Self {
            duration_ms,
            tracks: HashMap::new(),
        }
    }

    /// 保持する時間窓 (ミリ秒) を返す
    pub fn duration_ms(&self) -> u64 {
        self.duration_ms
    }

    /// publish した Object を保持する
    ///
    /// 同じ Location を再度保持した場合は置き換える。`properties` は
    /// [`LocProperties::encode`] の結果をそのまま保持する。同メソッドは Properties Length
    /// varint を含む完全なブロックを返すため (空の場合は長さ 0 の 1 バイト)、FETCH 応答の
    /// Object にそのまま渡せる。
    ///
    /// 期限切れの評価はここでも行う。FETCH 応答の判断時だけだと、窓を過ぎた Object を
    /// 保持し続けて告知した `MAX_CACHE_DURATION` と実際の保持が食い違う。
    ///
    /// # Errors
    ///
    /// LOC Properties のエンコードに失敗した場合はエラーになる。
    pub fn insert(
        &mut self,
        namespace: &TrackNamespace,
        track_name: &[u8],
        location: Location,
        properties: &LocProperties,
        payload: &[u8],
    ) -> Result<()> {
        self.evict_expired();

        // Properties Length varint を含む生バイト列 (Properties が空でも長さ 0 を含む)
        let properties_data = properties.encode()?;

        let now = Instant::now();
        let track = self
            .tracks
            .entry(namespace.clone())
            .or_default()
            .entry(track_name.to_vec())
            .or_default();
        let group = track
            .groups
            .entry(location.group_id)
            .or_insert_with(|| GroupCache {
                retained_at: now,
                objects: BTreeMap::new(),
            });
        // Group の保持期限は最後に保持した時刻から測る
        group.retained_at = now;
        group.objects.insert(
            location.object_id,
            CachedObject {
                location,
                properties_data,
                payload: payload.to_vec(),
            },
        );
        track.largest = Some(match track.largest {
            Some(largest) => largest.max(location),
            None => location,
        });
        track.first_published = Some(match track.first_published {
            Some(first) => first.min(location),
            None => location,
        });
        Ok(())
    }

    /// publish 済みの Largest Object を返す (1 件も publish していなければ `None`)
    ///
    /// evict 後も保持する。FETCH の要求 range を Largest Object でクランプするために使う。
    pub fn largest(&self, namespace: &TrackNamespace, track_name: &[u8]) -> Option<Location> {
        self.tracks
            .get(namespace)
            .and_then(|tracks| tracks.get(track_name))
            .and_then(|track| track.largest)
    }

    /// FETCH への応答内容を決める
    ///
    /// `published` はこの example がその track を PUBLISH しているかどうかである。保持内容だけでは
    /// 「publish していない track」と「まだ 1 件も publish していない track」を区別できないため、
    /// 呼び出し側が渡す。`published` が真でも、その track の Object を 1 件も保持していなければ
    /// (一度も publish していない) `UnknownTrack` を返す。
    ///
    /// `start` / `end` は要求の LOCATION_FILTER を解決した要求 range で、`end` は publish 済みの
    /// Largest Object でクランプした値である。Session が受理した FETCH の要求 range は空でない
    /// (`start <= end`) ため、ここでは検証しない。
    ///
    /// 期限切れの評価はここでも行う。追加時だけだと、窓を過ぎた Object を配り得る。
    pub fn plan(
        &mut self,
        published: bool,
        namespace: &TrackNamespace,
        track_name: &[u8],
        group_order: Option<u8>,
        start: Location,
        end: Location,
    ) -> FetchPlan<'_> {
        if !published {
            return FetchPlan::UnknownTrack;
        }
        // draft-ietf-moq-transport-22 §9.20.8 (GROUP ORDER Parameter) / §3.2.1 (Fetch):
        // 要求された group order で送る MUST があるため、Descending の要求に昇順の
        // 保持範囲を返さない
        if group_order == Some(GROUP_ORDER_DESCENDING) {
            return FetchPlan::UnsupportedGroupOrder;
        }
        self.evict_expired();
        let Some(track) = self
            .tracks
            .get(namespace)
            .and_then(|tracks| tracks.get(track_name))
        else {
            return FetchPlan::UnknownTrack;
        };

        // 一度も publish していない範囲は「存在しない」ため、End of Range を書かずに
        // FETCH_HEADER だけの stream を返す (draft-ietf-moq-transport-22 §3.2.1)
        let Some(first_published) = track.first_published else {
            return FetchPlan::NoPublishedObjects;
        };
        if end < first_published {
            return FetchPlan::EmptyRange;
        }

        let mut objects = Vec::new();
        for group in track.groups.values() {
            for object in group.objects.values() {
                if object.location >= start && object.location <= end {
                    objects.push(object);
                }
            }
        }
        // 要求 Start が最初に publish した Object より前の場合、その区間は「存在しない」。
        // 境界を End of Non-Existent Range で示す (続く End of Unknown Range が
        // 存在しない範囲まで「不明」に含めないため)。名前を付けられる境界は
        // 「最初に publish した Group の 1 つ前の Group」の末尾である
        let leading_non_existent = if start < first_published {
            let boundary = Location {
                group_id: first_published.group_id.saturating_sub(1),
                object_id: u64::MAX,
            };
            (boundary >= start).then_some(boundary)
        } else {
            None
        };
        // evict 済みの範囲 [max(start, first_published), last_evicted] を不明として示す
        let evicted_end = track
            .last_evicted
            .filter(|evicted| *evicted >= start.max(first_published) && *evicted <= end);

        // Group と Object は ID の昇順に保持しているため、この順で Location の昇順になる
        let Some(first) = objects.first() else {
            // 要求 range の Object を 1 つも保持していない
            let Some(evicted) = evicted_end else {
                // 一度も publish していない範囲しか無い (まだ配信していない側)
                return FetchPlan::EmptyRange;
            };
            // 要求 range の終端と evict 済みの終端の小さい方を不明の終端にする
            let end_location = end.min(evicted);
            return match leading_non_existent {
                // 未 publish の範囲と evict 済みの範囲が混在する。境界を分けて示す
                Some(non_existent) => FetchPlan::Serve {
                    leading_non_existent: Some(non_existent),
                    leading_unknown: Some(end_location),
                    objects: Vec::new(),
                },
                // evict 済みの範囲だけである
                None => FetchPlan::AllUnknown { end_location },
            };
        };
        // 最初の保持 Object より前の evict 済み範囲だけを不明として示す
        let leading_unknown = evicted_end.filter(|evicted| *evicted < first.location);
        FetchPlan::Serve {
            leading_non_existent,
            leading_unknown,
            objects,
        }
    }

    /// 時間窓を過ぎた Group を track ごとに evict する
    ///
    /// 古い側から順に、窓を過ぎた Group だけを落とす。時間窓で evict されるのは古い側だけ
    /// であるため、期限切れでない Group に当たった時点で止める (保持範囲の内部に欠落を
    /// 作らない)。
    fn evict_expired(&mut self) {
        let duration = Duration::from_millis(self.duration_ms);
        for tracks in self.tracks.values_mut() {
            for track in tracks.values_mut() {
                while let Some((&group_id, group)) = track.groups.first_key_value() {
                    if group.retained_at.elapsed() < duration {
                        break;
                    }
                    // evict した Group の最後 (最も大きい) Location を覚える
                    let last_object_id = group.objects.keys().next_back().copied();
                    track.groups.remove(&group_id);
                    if let Some(object_id) = last_object_id {
                        let location = Location {
                            group_id,
                            object_id,
                        };
                        track.last_evicted = Some(match track.last_evicted {
                            Some(evicted) => evicted.max(location),
                            None => location,
                        });
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use shiguredo_moqt::loc::{LocProperty, LocPropertyValue, PROP_TIMESTAMP};
    use shiguredo_moqt::name;

    /// テストで使う映像トラック名
    const TRACK: &[u8] = b"video";

    /// 期限切れの判定を実時間で確認するときの時間窓 (ミリ秒)
    ///
    /// 窓を過ぎた Object と窓に収まっている Object を 1 つの保持に同居させるには実時間を
    /// 進める必要がある。窓に収まっている側の判定が負荷で揺れないよう、窓より十分長い
    /// 余裕を残して待つ。
    const SHORT_WINDOW_MS: u64 = 250;

    /// 時間窓を過ぎるまで待つ
    fn wait_for_expiry() {
        std::thread::sleep(std::time::Duration::from_millis(SHORT_WINDOW_MS + 50));
    }

    /// テストで使う namespace
    fn namespace() -> TrackNamespace {
        name::parse_namespace("moq-example").expect("テスト用の namespace をパースできること")
    }

    /// Location を組み立てる
    fn loc(group_id: u64, object_id: u64) -> Location {
        Location {
            group_id,
            object_id,
        }
    }

    /// テストで使う LOC Properties (TIMESTAMP 1 つ)
    fn properties(timestamp: u64) -> LocProperties {
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(timestamp),
        });
        props
    }

    /// Object を保持する (失敗はテストの失敗として扱う)
    fn insert(cache: &mut MediaCache, location: Location, payload: &[u8]) {
        cache
            .insert(
                &namespace(),
                TRACK,
                location,
                &properties(location.object_id),
                payload,
            )
            .expect("Object を保持できること");
    }

    /// `Serve` の内容を取り出す (他の結果はテストの失敗として扱う)
    ///
    /// 返り値は (End of Non-Existent Range, End of Unknown Range, Object の並び) である。
    fn serve_parts<'a>(
        plan: FetchPlan<'a>,
    ) -> (Option<Location>, Option<Location>, Vec<&'a CachedObject>) {
        match plan {
            FetchPlan::Serve {
                leading_non_existent,
                leading_unknown,
                objects,
            } => (leading_non_existent, leading_unknown, objects),
            other => panic!("Serve になること: {other:?}"),
        }
    }

    /// Location の並びを取り出す
    fn locations(objects: &[&CachedObject]) -> Vec<Location> {
        objects.iter().map(|object| object.location).collect()
    }

    /// PUBLISH していない track は対象外として扱うこと
    ///
    /// Session は publish 実績の無い track の FETCH を受理するため、応答の判断で
    /// `REQUEST_DOES_NOT_EXIST` を返す経路が必要になる。
    #[test]
    fn plan_reports_an_unpublished_track_as_unknown() {
        let mut cache = MediaCache::new(60_000);
        insert(&mut cache, loc(1, 0), b"video");

        let plan = cache.plan(true, &namespace(), b"audio", None, loc(1, 0), loc(1, 0));
        assert!(
            matches!(plan, FetchPlan::UnknownTrack),
            "保持が無い track は対象外として扱うこと: {plan:?}"
        );

        let plan = cache.plan(false, &namespace(), TRACK, None, loc(1, 0), loc(1, 0));
        assert!(
            matches!(plan, FetchPlan::UnknownTrack),
            "PUBLISH していない track は保持があっても対象外として扱うこと: {plan:?}"
        );
    }

    /// 対象外の track の判定は GROUP_ORDER の判定より先であること
    ///
    /// 判定の順序は「対象外の track」→「GROUP_ORDER」→「保持内容」である。順序を入れ替えると、
    /// 応答できない track に `REQUEST_NOT_SUPPORTED` を返してしまう。
    #[test]
    fn plan_checks_the_track_before_the_group_order() {
        let mut cache = MediaCache::new(60_000);

        let plan = cache.plan(false, &namespace(), TRACK, Some(0x02), loc(0, 0), loc(0, 0));
        assert!(
            matches!(plan, FetchPlan::UnknownTrack),
            "対象外の track を先に判定すること: {plan:?}"
        );
    }

    /// GROUP_ORDER が Descending の要求は応答しないこと
    ///
    /// draft-ietf-moq-transport-22 §3.2.1 (Fetch) は要求された group order で送る MUST を
    /// 課すため、昇順の保持範囲を返さず `REQUEST_NOT_SUPPORTED` の判断にする。
    #[test]
    fn plan_rejects_a_descending_group_order() {
        let mut cache = MediaCache::new(60_000);
        insert(&mut cache, loc(1, 0), b"video");

        let plan = cache.plan(true, &namespace(), TRACK, Some(0x02), loc(1, 0), loc(1, 0));
        assert!(
            matches!(plan, FetchPlan::UnsupportedGroupOrder),
            "Descending では昇順の保持範囲を返さないこと: {plan:?}"
        );
    }

    /// 保持している Object を要求 range 内だけ Location の昇順で返すこと
    #[test]
    fn plan_returns_the_retained_objects_inside_the_range() {
        let mut cache = MediaCache::new(60_000);
        insert(&mut cache, loc(1, 0), b"a");
        insert(&mut cache, loc(1, 1), b"b");
        insert(&mut cache, loc(2, 0), b"c");

        // 要求 Start は保持の先頭と一致するため End of Unknown Range を置かない
        let plan = cache.plan(true, &namespace(), TRACK, Some(0x01), loc(1, 1), loc(2, 0));
        let (_, leading_unknown, objects) = serve_parts(plan);
        assert_eq!(
            leading_unknown, None,
            "要求 range 内に evict が無ければ End of Unknown Range を置かないこと"
        );
        assert_eq!(
            locations(&objects),
            vec![loc(1, 1), loc(2, 0)],
            "要求 range 内の Object だけを Location の昇順で返すこと"
        );
    }

    /// 保持より前の範囲には evict が無ければ End of Unknown Range を置かないこと
    ///
    /// 要求 Start が最初に返す Object より前でも、その範囲の Object を保持したことが無ければ
    /// 「不明」ではなく既定の「存在しない」として扱う (draft-ietf-moq-transport-22 §3.2.2)。
    #[test]
    fn plan_omits_end_of_unknown_range_without_an_evicted_object() {
        let mut cache = MediaCache::new(60_000);
        insert(&mut cache, loc(5, 0), b"a");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(1, 0), loc(5, 0));
        let (_, leading_unknown, objects) = serve_parts(plan);
        assert_eq!(
            leading_unknown, None,
            "保持したことの無い範囲に End of Unknown Range を置かないこと"
        );
        assert_eq!(
            locations(&objects),
            vec![loc(5, 0)],
            "保持 Object を返すこと"
        );
    }

    /// 要求 Start 側が evict 済みなら、最初の Object の前に End of Unknown Range を置くこと
    ///
    /// draft-ietf-moq-transport-22 §11.4.1.2 (End of Range) の End of Unknown Range は
    /// 「直前に書いた entry の Location から自分の Location までの区間」を表す。最初の entry に
    /// 置く場合は要求 range の先頭から自分の Location までが不明であることを表すため、
    /// Location は要求 range 内で最後に evict した Location にする。
    #[test]
    fn plan_puts_end_of_unknown_range_before_the_first_object() {
        let mut cache = MediaCache::new(SHORT_WINDOW_MS);
        // 窓を過ぎた Group (evict される)
        insert(&mut cache, loc(1, 0), b"a");
        insert(&mut cache, loc(1, 1), b"b");
        wait_for_expiry();
        // 新しい Group。この保持で古い Group が evict される
        insert(&mut cache, loc(2, 0), b"c");
        insert(&mut cache, loc(2, 1), b"d");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(1, 0), loc(2, 1));
        let (_, leading_unknown, objects) = serve_parts(plan);
        assert_eq!(
            leading_unknown,
            Some(loc(1, 1)),
            "要求 range 内で最後に evict した Location を置くこと"
        );
        assert_eq!(
            locations(&objects),
            vec![loc(2, 0), loc(2, 1)],
            "evict 済みの Object は返さないこと"
        );
    }

    /// 要求 Start が保持の先頭と一致する場合は End of Unknown Range を置かないこと
    #[test]
    fn plan_omits_end_of_unknown_range_when_the_start_is_retained() {
        let mut cache = MediaCache::new(SHORT_WINDOW_MS);
        insert(&mut cache, loc(1, 0), b"a");
        wait_for_expiry();
        insert(&mut cache, loc(2, 0), b"b");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(2, 0), loc(2, 0));
        let (_, leading_unknown, objects) = serve_parts(plan);
        assert_eq!(
            leading_unknown, None,
            "要求 Start が最初に返す Object と同じなら End of Unknown Range を置かないこと"
        );
        assert_eq!(
            locations(&objects),
            vec![loc(2, 0)],
            "保持 Object を返すこと"
        );
    }

    /// 一度も publish していない範囲だけを要求された場合は空の応答にすること
    ///
    /// 存在しない Object に End of Unknown Range を書かない (draft-ietf-moq-transport-22
    /// §3.2.1 は「要求 range に Object が無い」場合に FETCH_HEADER だけの stream を返すと定める)。
    #[test]
    fn plan_reports_a_range_before_the_first_object_as_empty() {
        let mut cache = MediaCache::new(60_000);
        insert(&mut cache, loc(10, 0), b"a");

        // 最初に publish した Group より前だけを要求する
        let plan = cache.plan(true, &namespace(), TRACK, None, loc(4, 0), loc(5, 0));
        assert!(
            matches!(plan, FetchPlan::EmptyRange),
            "未 publish の範囲は空の応答になること: {plan:?}"
        );
    }

    /// 未 publish の範囲と evict 済みの範囲が混在する場合は境界を分けて示すこと
    ///
    /// End of Non-Existent Range は「存在しない範囲と不明な範囲に分割する」用途に限って
    /// 使ってよい (draft-ietf-moq-transport-22 §3.2.2)。分割しないと、存在しない範囲まで
    /// 「不明」と主張してしまう。
    #[test]
    fn plan_splits_non_existent_and_evicted_ranges() {
        let mut cache = MediaCache::new(0);
        insert(&mut cache, loc(10, 0), b"a");
        insert(&mut cache, loc(10, 1), b"b");
        insert(&mut cache, loc(11, 0), b"c");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(4, 0), loc(11, 0));
        let (non_existent, unknown, objects) = serve_parts(plan);
        assert!(objects.is_empty(), "保持は無いこと");
        assert_eq!(
            non_existent,
            Some(loc(9, u64::MAX)),
            "未 publish の範囲の境界を示すこと"
        );
        assert_eq!(
            unknown,
            Some(loc(11, 0)),
            "evict 済みの範囲を不明として示すこと"
        );
    }

    /// 要求 range 全体が evict 済みである場合は不明として示すこと
    ///
    /// evict 済みは「存在しない」ではなく「不明」である (draft-ietf-moq-transport-22 §2.1.2 /
    /// §10.3)。End of Unknown Range の Location は FETCH_OK の End Location にする。
    #[test]
    fn plan_reports_an_evicted_range_as_unknown() {
        let mut cache = MediaCache::new(0);
        insert(&mut cache, loc(10, 0), b"a");
        insert(&mut cache, loc(10, 1), b"b");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(10, 0), loc(10, 1));
        match plan {
            FetchPlan::AllUnknown { end_location } => {
                assert_eq!(end_location, loc(10, 1), "End Location に置くこと");
            }
            other => panic!("AllUnknown になること: {other:?}"),
        }
    }

    /// 応答に書く entry の並びを固定すること
    ///
    /// End of Range は「直前に書いた entry の Location から自分の Location までの区間」を
    /// 表す位置依存の表記であるため (draft-ietf-moq-transport-22 §11.4.1.2)、並びそのものを
    /// 固定する。存在しない範囲 → 不明な範囲 → Object の順になる。
    #[test]
    fn plan_entries_are_written_in_order() {
        let mut cache = MediaCache::new(SHORT_WINDOW_MS);
        // 窓を過ぎて evict される Object
        insert(&mut cache, loc(10, 0), b"old");
        wait_for_expiry();
        // 保持される Object
        insert(&mut cache, loc(12, 0), b"new");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(4, 0), loc(12, 0));
        let entries = plan.entries();
        match entries.as_slice() {
            [
                FetchResponseEntry::NonExistent(non_existent),
                FetchResponseEntry::Unknown(unknown),
                FetchResponseEntry::Object(object),
            ] => {
                assert_eq!(*non_existent, loc(9, u64::MAX), "未 publish の範囲の境界");
                assert_eq!(*unknown, loc(10, 0), "evict 済みの範囲");
                assert_eq!(object.location, loc(12, 0), "保持している Object");
            }
            other => panic!("NonExistent → Unknown → Object の順であること: {other:?}"),
        }
    }

    /// 欠落が無い範囲では End of Range を書かないこと
    #[test]
    fn plan_entries_skip_markers_without_gaps() {
        let mut cache = MediaCache::new(60_000);
        insert(&mut cache, loc(10, 0), b"a");
        insert(&mut cache, loc(10, 1), b"b");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(10, 0), loc(10, 1));
        let entries = plan.entries();
        assert_eq!(entries.len(), 2, "Object 2 件だけであること: {entries:?}");
        for entry in &entries {
            assert!(
                matches!(entry, FetchResponseEntry::Object(_)),
                "End of Range を書かないこと: {entry:?}"
            );
        }
    }

    /// 応答を書かない判断では entry が空であること
    #[test]
    fn plan_entries_are_empty_without_a_response_body() {
        let mut cache = MediaCache::new(60_000);
        insert(&mut cache, loc(10, 0), b"a");

        // 未 publish の範囲 (FETCH_HEADER だけを書く)
        let plan = cache.plan(true, &namespace(), TRACK, None, loc(4, 0), loc(5, 0));
        assert!(
            plan.entries().is_empty(),
            "空の応答では entry を書かないこと"
        );

        // 対象外の track (応答しない)
        let plan = cache.plan(false, &namespace(), TRACK, None, loc(10, 0), loc(10, 0));
        assert!(plan.entries().is_empty(), "対象外では entry を書かないこと");

        // Descending (拒否する)
        let plan = cache.plan(
            true,
            &namespace(),
            TRACK,
            Some(0x02),
            loc(10, 0),
            loc(10, 0),
        );
        assert!(plan.entries().is_empty(), "拒否では entry を書かないこと");
    }

    /// LOC Properties のエンコードに失敗した場合は保持を変えないこと
    ///
    /// `insert` は期限切れの評価のあとにエンコードするため、エラー時に保持だけが
    /// 中途半端に変わることはない (`# Errors` の契約)。
    #[test]
    fn insert_rejects_invalid_properties_without_changing_the_cache() {
        let mut cache = MediaCache::new(60_000);
        insert(&mut cache, loc(1, 0), b"a");

        // 同じ prop_id を 2 度積むと encode がエラーになる (draft-ietf-moq-transport-22 §11.1.2)
        let mut props = LocProperties::new();
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(1),
        });
        props.push(LocProperty {
            prop_id: PROP_TIMESTAMP,
            value: LocPropertyValue::VarInt(2),
        });
        let result = cache.insert(&namespace(), TRACK, loc(2, 0), &props, b"c");
        assert!(result.is_err(), "不正な Properties はエラーにすること");
        assert_eq!(
            cache.largest(&namespace(), TRACK),
            Some(loc(1, 0)),
            "Largest Object は変わらないこと"
        );
        let plan = cache.plan(true, &namespace(), TRACK, None, loc(1, 0), loc(2, 0));
        let (_, _, objects) = serve_parts(plan);
        assert_eq!(
            locations(&objects),
            vec![loc(1, 0)],
            "保持している Object は変わらないこと"
        );
    }

    /// 期限切れの判定は FETCH 応答の判断時にも行うこと
    ///
    /// 保持への追加時だけ評価すると、窓を過ぎた Object を配り得る。Largest Object は
    /// evict 後も残る (要求 range のクランプに使う)。
    #[test]
    fn plan_evicts_expired_objects() {
        let mut cache = MediaCache::new(SHORT_WINDOW_MS);
        insert(&mut cache, loc(1, 0), b"a");
        insert(&mut cache, loc(1, 1), b"b");
        wait_for_expiry();

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(1, 0), loc(1, 1));
        assert!(
            matches!(plan, FetchPlan::AllUnknown { end_location } if end_location == loc(1, 1)),
            "窓を過ぎた Object は配らないこと: {plan:?}"
        );
        assert_eq!(
            cache.largest(&namespace(), TRACK),
            Some(loc(1, 1)),
            "Largest Object は evict 後も保持すること"
        );
    }

    /// 期限切れの判定は保持への追加時にも行うこと
    ///
    /// 追加時に評価しないと、窓を過ぎた Object を保持し続けて告知した `MAX_CACHE_DURATION` と
    /// 実際の保持が食い違う。FETCH の判断を待たずに保持量を抑えていることを、保持の内部状態で
    /// 固定する (時間窓 0 は次の保持で必ず期限切れになるため実時間を待たない)。
    #[test]
    fn insert_evicts_expired_objects() {
        let mut cache = MediaCache::new(0);
        insert(&mut cache, loc(1, 0), b"a");
        insert(&mut cache, loc(2, 0), b"b");

        let track = cache
            .tracks
            .get(&namespace())
            .and_then(|tracks| tracks.get(TRACK))
            .expect("保持した track があること");
        assert_eq!(
            track.groups.len(),
            1,
            "追加時に窓を過ぎた Group を落とすこと"
        );
        assert_eq!(
            track.last_evicted,
            Some(loc(1, 0)),
            "evict した Group の最後の Location を覚えること"
        );
        assert_eq!(cache.duration_ms(), 0, "保持する時間窓を返すこと");
    }

    /// 保持する Properties が Properties Length varint を含む生バイト列であること
    ///
    /// FETCH 応答の Object は Properties フィールドを持つため、Properties Length varint を
    /// 含む完全なブロックとして保持する (draft-ietf-moq-transport-22 §11.1.2 (Object
    /// Properties))。保持したバイト列をそのままデコードできることで、長さの前置が 1 回だけ
    /// であることを固定する。
    #[test]
    fn insert_stores_properties_with_the_length_prefix() {
        let mut cache = MediaCache::new(60_000);
        let props = properties(1_234);
        cache
            .insert(&namespace(), TRACK, loc(1, 0), &props, b"payload")
            .expect("Object を保持できること");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(1, 0), loc(1, 0));
        let (_, _, objects) = serve_parts(plan);
        assert_eq!(
            objects[0].properties_data,
            props
                .encode()
                .expect("LOC Properties をエンコードできること"),
            "Properties Length varint を含むブロックをそのまま保持すること"
        );
        let (decoded, consumed) = LocProperties::decode(&objects[0].properties_data)
            .expect("保持した Properties をデコードできること");
        assert_eq!(
            consumed,
            objects[0].properties_data.len(),
            "保持したバイト列ちょうどを消費すること (余分な前置が無いこと)"
        );
        assert_eq!(decoded, props, "保持した Properties が元の値と一致すること");
        assert_eq!(
            objects[0].payload, b"payload",
            "payload も保持すること (FETCH 応答には両方が要る)"
        );
    }

    /// Properties が空でも Properties Length 0 を保持すること
    #[test]
    fn insert_keeps_the_zero_length_prefix_for_empty_properties() {
        let mut cache = MediaCache::new(60_000);
        cache
            .insert(
                &namespace(),
                TRACK,
                loc(1, 0),
                &LocProperties::new(),
                b"payload",
            )
            .expect("Object を保持できること");

        let plan = cache.plan(true, &namespace(), TRACK, None, loc(1, 0), loc(1, 0));
        let (_, _, objects) = serve_parts(plan);
        assert_eq!(
            objects[0].properties_data,
            vec![0x00],
            "空の Properties でも長さ 0 を含むこと"
        );
    }

    /// 保持しない時間窓 (0 ミリ秒) でも Largest Object は残ること
    ///
    /// 0 は「保持しない」を意味する。FETCH は受理されて応答できるが、配れる Object は無い。
    #[test]
    fn zero_duration_keeps_only_the_largest_object() {
        let mut cache = MediaCache::new(0);
        insert(&mut cache, loc(1, 0), b"a");
        insert(&mut cache, loc(2, 0), b"b");

        assert_eq!(
            cache.largest(&namespace(), TRACK),
            Some(loc(2, 0)),
            "Largest Object は evict 後も保持すること"
        );
        let plan = cache.plan(true, &namespace(), TRACK, None, loc(1, 0), loc(2, 0));
        assert!(
            matches!(plan, FetchPlan::AllUnknown { end_location } if end_location == loc(2, 0)),
            "保持しない時間窓では常に不明として返すこと: {plan:?}"
        );
    }
}

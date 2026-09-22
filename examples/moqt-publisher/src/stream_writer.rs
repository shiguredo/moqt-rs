//! Subgroup Stream ライター
//!
//! 1 つの Group に対応する一方向ストリームへオブジェクトを書き込む。
//! draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §11.3.2 (Closing Subgroup Streams) に準拠する。

use bytes::Bytes;

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::{
    message::common::Location, session::types::DataStreamId, session::types::DataStreamResetReason,
    session::types::RequestStreamEnd, stream::subgroup::SubgroupHeader,
    stream::subgroup::SubgroupIdMode, stream::subgroup::SubgroupObject,
};

use crate::error::Result;
use moqt_example_transport::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use moqt_example_transport::transport;

/// Subgroup Stream の終端方法 (sans I/O)
///
/// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) の MUST に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubgroupTermination {
    /// 配送すべき Object をすべて配送したため FIN で閉じる
    Fin,
    /// 配送すべき Object を配送せずに閉じるため RESET_STREAM で閉じる
    Reset,
}

impl SubgroupTermination {
    /// RESET で閉じる場合の Stream Reset Error Code (FIN の場合は `None`)
    ///
    /// 購読者側のフィルタ不通過・Start Location や End Group の更新による配送中止のいずれも
    /// 「送信側の都合で配送を中止した」ため CANCELLED を使う
    /// (draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes))。
    fn reset_error_code(self) -> Option<u64> {
        match self {
            SubgroupTermination::Fin => None,
            SubgroupTermination::Reset => Some(DataStreamResetReason::Cancelled.error_code()),
        }
    }
}

/// Subgroup の Object ID 追跡とヘッダ送信の状態 (sans I/O)
///
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §2.2 (Subgroups): FIRST_OBJECT bit は
/// 「その subgroup stream の最初の Object が original publisher により subgroup に公開された
/// 最初の Object である」ことを示す。Object 0 がフィルタ不通過でスキップされた場合は false に
/// する必要があるため、最初の Pass までヘッダ送信を遅延して確定させる。
///
/// 終端方法も併せて追跡する。判定規則は [`SubgroupObjectState::termination`] を参照。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
#[derive(Debug)]
struct SubgroupObjectState {
    /// Subgroup の Group ID (省略した Object の Location を組み立てるために使う)
    group_id: u64,
    /// 次に送信を試みる Object ID
    next_object_id: u64,
    /// 直前の Pass 済み Object ID (デルタ計算用)
    prev_object_id: Option<u64>,
    /// ワイヤへのヘッダ送信を決めたか
    header_sent: bool,
    /// Subgroup 開始時点の Start Location の snapshot (`None` は Location Filter 無し)
    start_location: Option<Location>,
    /// 省略した Object のうち最後の (最も大きい) Location
    ///
    /// 省略する Object の Location は単調増加するため、最大値だけを保持すれば
    /// 「省略した Object のうち Start Location 以降のものがあるか」を判定できる。
    max_skipped_location: Option<Location>,
}

impl SubgroupObjectState {
    /// 初期状態 (Object ID 0・直前の Pass なし・Skip なし) を返す
    ///
    /// `start_location` には Subgroup 開始時点の購読の Start Location を渡す (判定規則は
    /// [`SubgroupObjectState::termination`] を参照)。
    fn new(group_id: u64, start_location: Option<Location>) -> Self {
        Self {
            group_id,
            next_object_id: 0,
            prev_object_id: None,
            header_sent: false,
            start_location,
            max_skipped_location: None,
        }
    }

    /// 次に送信を試みる Object ID
    fn current_object_id(&self) -> u64 {
        self.next_object_id
    }

    /// 次に送信する Object の Object ID Delta を返す
    ///
    /// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): 最初の Object は絶対 ID を送り、
    /// 以降は直前の Pass 済み Object ID との差から 1 を引いた Object ID Delta を送る
    /// (受信側は前の Object ID に delta + 1 を加算して絶対 ID を復元する)。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    fn current_object_id_delta(&self) -> u64 {
        match self.prev_object_id {
            None => self.next_object_id,
            Some(prev) => self.next_object_id - prev - 1,
        }
    }

    /// フィルタ不通過の Object を記録して次の ID へ進める
    ///
    /// 省略の理由 (Forward State 0 / Location Filter / Range Filter) は区別しない。
    /// 呼び出し側は Object ID の昇順で呼ぶため、保持するのは最後の Location だけでよい。
    fn on_skip(&mut self) {
        self.max_skipped_location = Some(Location {
            group_id: self.group_id,
            object_id: self.next_object_id,
        });
        self.next_object_id += 1;
    }

    /// Pass した Object を記録し、最初の Pass なら送信すべき `first_object` を返す
    fn on_pass(&mut self) -> Option<bool> {
        let object_id = self.next_object_id;
        let first_object = if self.header_sent {
            None
        } else {
            self.header_sent = true;
            Some(object_id == 0)
        };
        self.prev_object_id = Some(object_id);
        self.next_object_id = object_id + 1;
        first_object
    }

    /// Subgroup の終端方法を決める
    ///
    /// 次のいずれかに該当すれば RESET、すべて該当しなければ FIN である。
    ///
    /// - ワイヤへ `SUBGROUP_HEADER` を一度も送っていない (最初の Object が Pass しなかった)。
    ///   Subgroup を開始したが配送可能な Object が 1 つも無かった場合であり、購読者へ空の
    ///   Subgroup を通知する意味が無いため RESET で閉じる (RESET_STREAM_AT が使えない制約は
    ///   [`SubgroupWriter::finish`] を参照)
    /// - 省略した Object が 1 つでも **Subgroup 開始時点の** Start Location 以降の Location を持つ
    /// - 省略した Object が 1 つでも **終端時点の** Start Location 以降の Location を持つ
    /// - Start Location を持たない (unfiltered) 時点がある購読で 1 つでも省略した
    ///
    /// 1 つ目と 2 つ目の条件は §11.3.2 の FIN 条件 ("If a sender has delivered all objects in a
    /// Subgroup to the QUIC stream, except any Objects with Locations smaller than the
    /// subscription's Start Location, it MUST close the stream with a FIN.") を写したものである。
    /// 例外は "smaller than" のみなので、Start Location と同じ Location の Object も配送対象に
    /// 含まれる (省略した Object の Location は昇順なので、最後に省略した Location だけを見る)。
    ///
    /// Start Location は REQUEST_UPDATE で変わりうるため、**開始時点の snapshot** と
    /// **終端時点の現在値** の両方で判定し、省略した Object がどちらかの時点で配送対象だったなら
    /// RESET 側に倒す。比較は Group 込みの絶対 Location (`(group_id, object_id)` の辞書順) で
    /// 行う。前者は §11.3.2 が RESET の例に挙げる Start Location の拡大
    /// ("A REQUEST_UPDATE moving the subscription's End Group to a smaller Group or the Start
    /// Location to a larger Location") を捉え、後者は §9.5.1 (Updating Subscriptions) が想定する
    /// 縮小・フィルタ削除 ("When a subscriber decreases the Start Location of the Location Filter
    /// ... the Start Location can be smaller than the Track's Largest Location") を捉える。
    /// どちらも FIN を送ると Subgroup を完備と誤認させ、欠落分の FETCH が行われなくなる。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    fn termination(&self, current_start_location: Option<Location>) -> SubgroupTermination {
        if !self.header_sent {
            return SubgroupTermination::Reset;
        }
        let Some(max_skipped) = self.max_skipped_location else {
            return SubgroupTermination::Fin;
        };
        // どちらかの時点で Location Filter が無いなら、省略した Object はすべて配送対象だった
        let (Some(start), Some(current)) = (self.start_location, current_start_location) else {
            return SubgroupTermination::Reset;
        };
        if max_skipped < start.min(current) {
            SubgroupTermination::Fin
        } else {
            SubgroupTermination::Reset
        }
    }
}

/// Subgroup Stream ライター
///
/// 1 つの Group (= GOP) に対応する一方向ストリームにオブジェクトを書き込む。
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §11.3.2 (Closing Subgroup Streams)
pub struct SubgroupWriter {
    stream: transport::SendStream,
    data_plane: DataPlaneHandle,
    stream_id: DataStreamId,
    /// ヘッダ再構築用の Track Alias
    track_alias: u64,
    /// ヘッダ再構築用の Group ID
    group_id: u64,
    /// ヘッダ再構築用の Publisher Priority
    publisher_priority: u8,
    /// SUBGROUP_HEADER の PROPERTIES ビット
    has_properties: bool,
    /// Object ID 追跡・FIRST_OBJECT bit・終端方法の決定状態
    object_state: SubgroupObjectState,
}

impl SubgroupWriter {
    /// 新しい Subgroup Stream を開き、Session に登録する
    ///
    /// ワイヤへの SUBGROUP_HEADER 送信は最初の Pass まで遅延し、`first_object` は最初の Pass が
    /// Object 0 かどうかで確定する (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §2.2 (Subgroups))。
    /// Session 登録 (`send_subgroup_header`) は `send_subgroup_object` のフィルタ評価と
    /// `send_data_stream_closed` の前提になるため、`new` の時点で行う。
    ///
    /// `start_location` は Subgroup 開始時点の購読の Start Location
    /// (`MoqtClient::subscription_filter_start`)。
    #[expect(
        clippy::too_many_arguments,
        reason = "Subgroup を開くために必要なヘッダ情報とフィルタ判定情報を 1 回で渡すため"
    )]
    pub async fn new(
        handle: &transport::StreamHandle,
        data_plane: &DataPlaneHandle,
        request_id: u64,
        track_alias: u64,
        group_id: u64,
        publisher_priority: u8,
        has_properties: bool,
        start_location: Option<Location>,
    ) -> Result<Self> {
        let stream = handle.open_send_stream().await?;
        let stream_id = DataStreamId(stream.stream_id());

        let writer = Self {
            stream,
            data_plane: data_plane.clone(),
            stream_id,
            track_alias,
            group_id,
            publisher_priority,
            has_properties,
            object_state: SubgroupObjectState::new(group_id, start_location),
        };
        // Session 登録用のヘッダ。first_object は Session では参照されないため、
        // ワイヤ送信時に確定した値で組み立て直す
        let header = writer.build_header(false);
        data_plane.send_subgroup_header(stream_id, request_id, &header)?;
        Ok(writer)
    }

    /// ヘッダ再構築用の値から SUBGROUP_HEADER を組み立てる
    fn build_header(&self, first_object: bool) -> SubgroupHeader {
        SubgroupHeader {
            track_alias: self.track_alias,
            group_id: self.group_id,
            subgroup_id: SubgroupIdMode::Zero,
            publisher_priority: Some(self.publisher_priority),
            has_properties: self.has_properties,
            end_of_group: false,
            first_object,
        }
    }

    /// オブジェクトを書き込む
    ///
    /// LOC プロパティとペイロードを含むオブジェクトをストリームに書き込む。
    /// Session のフィルタ評価を先に行い、通過時のみワイヤへ出す
    /// (draft-ietf-moq-transport-21 §3.3.3 (Combining Filters): "The publisher MUST forward
    /// only objects that pass all filters")。フィルタ不通過
    /// (`SendRequestError::LocalFilterMismatch`) のオブジェクトはワイヤに出さず `Skip` を返す。
    /// オブジェクト ID だけ進める (ID ギャップは draft §11.3.1 (Subgroup Header) の
    /// "A consumer cannot infer information about the existence of Objects between the current
    /// and previous Object ID in the Subgroup" により許容される。なおフィルタ由来の
    /// ギャップに PRIOR_OBJECT_ID_GAP (draft §10.9 (Prior Object ID Gap)) は付与しない: 同プロパティは任意であり、
    /// フィルタは REQUEST_UPDATE で変化しうるため確定情報を出せない以上、省略が最も安全)。
    /// 直前の Pass 済み Object ID はフィルタを通過したオブジェクトでのみ更新する
    /// (受信側は直前の受信オブジェクト ID に delta + 1 を足して絶対 ID を復元するため、
    /// スキップ時に進めると delta が 1 つ分小さくなり絶対 ID が静かにずれる。
    /// draft §11.3.1 (Subgroup Header): "The Object ID Delta + 1 is added to the previous Object ID in the
    /// Subgroup stream if there was one.")。
    ///
    /// 返り値の `ObjectFilterOutcome::Pass` はワイヤへ送信したこと、`Skip` はフィルタ
    /// 不通過で送信しなかったことを示す。呼び出し側は `Skip` をスキップとして扱い、
    /// 送信を継続する (その他のエラーは伝播する)。
    ///
    /// 検証 (手動): OBJECT_PROPERTY_FILTER 付きの subscription に対して、
    /// フィルタ外のオブジェクトが subscriber に届かず publisher が生存すること、
    /// フィルタを通過するオブジェクトの配信が従来どおり行われること (ID ギャップ後の
    /// 絶対 ID 復元を含む) を確認する。リポジトリ内の moqt-subscriber はフィルタ付き
    /// SUBSCRIBE を発行する手段を持たないため、 subscriber 側にフィルタを付与する
    /// 改変が必要である。
    pub async fn write_object(
        &mut self,
        payload: &[u8],
        properties: &LocProperties,
    ) -> Result<ObjectFilterOutcome> {
        let object_id = self.object_state.current_object_id();
        let object_id_delta = self.object_state.current_object_id_delta();

        // LOC プロパティをエンコードする
        let properties_data = if properties.is_empty() {
            None
        } else {
            Some(properties.encode()?)
        };

        // フィルタ評価は doc のとおりワイヤ送信より先に行う
        let outcome = self.data_plane.send_subgroup_object(
            self.stream_id,
            object_id,
            properties_data.as_deref(),
        )?;
        if outcome == ObjectFilterOutcome::Skip {
            tracing::debug!(
                "skip subgroup object (filter mismatch): stream_id={}, object_id={}",
                self.stream_id.0,
                object_id,
            );
            self.object_state.on_skip();
            return Ok(ObjectFilterOutcome::Skip);
        }

        // 最初の Pass まで SUBGROUP_HEADER を遅延し、FIRST_OBJECT bit を確定させる
        if let Some(first_object) = self.object_state.on_pass() {
            let header = self.build_header(first_object);
            let encoded = header.encode();
            self.stream.send(Bytes::from(encoded)).await?;
        }

        let obj = SubgroupObject {
            object_id_delta,
            payload_length: payload.len() as u64,
            status: None,
        };

        let mut buf = Vec::new();
        obj.encode(self.has_properties, properties_data.as_deref(), &mut buf)?;
        buf.extend_from_slice(payload);

        self.stream.send(Bytes::from(buf)).await?;

        Ok(ObjectFilterOutcome::Pass)
    }

    /// ストリームを終了する
    ///
    /// 終端方法は `SubgroupObjectState::termination` が決める (sans I/O)。RESET 側の場合は
    /// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) の MUST に従い reset で
    /// 終端する。
    ///
    /// `current_start_location` には終端時点の購読の Start Location を渡す
    /// (`MoqtClient::subscription_filter_start`)。
    ///
    /// draft は配送可能な Object が無い Subgroup を SUBGROUP_HEADER + RESET_STREAM_AT で通知する
    /// ことを MAY で示すが、example の transport API に RESET_STREAM_AT が無いためヘッダを送らず
    /// RESET_STREAM で代替する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn finish(mut self, current_start_location: Option<Location>) -> Result<()> {
        let termination = self.object_state.termination(current_start_location);
        tracing::debug!(
            "finish subgroup stream: stream_id={}, group_id={}, termination={:?}, start_location={:?}, \
             current_start_location={:?}, max_skipped_location={:?}",
            self.stream_id.0,
            self.group_id,
            termination,
            self.object_state.start_location,
            current_start_location,
            self.object_state.max_skipped_location,
        );
        // 判定から終端方法への写像は `SubgroupTermination::reset_error_code` が持つ
        // (`transport::SendStream` を要求する分岐そのものは example の単体テストから観測できない)
        match termination.reset_error_code() {
            Some(error_code) => {
                self.stream.reset(error_code)?;
                self.data_plane.send_data_stream_closed(
                    self.stream_id,
                    RequestStreamEnd::Reset {
                        error_code,
                        reliable_size: None,
                    },
                )?;
            }
            None => {
                self.stream.finish()?;
                self.data_plane
                    .send_data_stream_closed(self.stream_id, RequestStreamEnd::Fin)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DataStreamResetReason, SubgroupObjectState, SubgroupTermination};
    use shiguredo_moqt::message::common::Location;

    /// Group `group_id` / Object ID `object_id` の Location を返す
    fn loc(group_id: u64, object_id: u64) -> Location {
        Location {
            group_id,
            object_id,
        }
    }

    /// Group `group_id` の Subgroup と Start Location の状態を作る
    fn state_in_group(group_id: u64, start_location: Option<Location>) -> SubgroupObjectState {
        SubgroupObjectState::new(group_id, start_location)
    }

    /// Start Location を持たない (unfiltered) 購読の状態を作る
    fn state_unfiltered() -> SubgroupObjectState {
        state_in_group(7, None)
    }

    /// Start Location が `loc(7, object_id)` の状態を作る
    fn state_at(start_object_id: u64) -> SubgroupObjectState {
        state_in_group(7, Some(loc(7, start_object_id)))
    }

    // Object 0 が Pass した場合は FIRST_OBJECT を true で送り、FIN で終端する
    #[test]
    fn first_object_true_when_object_zero_passes() {
        let mut state = state_unfiltered();
        assert_eq!(state.current_object_id(), 0);
        assert_eq!(state.current_object_id_delta(), 0);
        assert_eq!(state.on_pass(), Some(true));
        assert_eq!(state.on_pass(), None);
        assert_eq!(state.termination(None), SubgroupTermination::Fin);
    }

    // Object 0 が Skip されて Object 1 が最初の Pass になった場合は FIRST_OBJECT を false で送る
    #[test]
    fn first_object_false_when_object_zero_is_skipped() {
        let mut state = state_unfiltered();
        state.on_skip();
        assert_eq!(state.current_object_id(), 1);
        // 最初の Pass は絶対 ID が delta になる
        assert_eq!(state.current_object_id_delta(), 1);
        assert_eq!(state.on_pass(), Some(false));
        assert_eq!(state.on_pass(), None);
        // Start Location が無い購読では Object 0 も配送対象なので reset 側になる
        assert_eq!(state.termination(None), SubgroupTermination::Reset);
    }

    // 連続して Pass した場合の delta は 0 になる
    #[test]
    fn object_id_delta_is_zero_for_consecutive_passes() {
        let mut state = state_unfiltered();
        assert_eq!(state.on_pass(), Some(true));
        assert_eq!(state.current_object_id_delta(), 0);
        assert_eq!(state.on_pass(), None);
    }

    // Pass の間に Skip が挟まっても delta は直前の Pass 済み ID 基準になる
    #[test]
    fn object_id_delta_counts_skipped_gap() {
        let mut state = state_unfiltered();
        assert_eq!(state.on_pass(), Some(true));
        state.on_skip();
        assert_eq!(state.current_object_id(), 2);
        assert_eq!(state.current_object_id_delta(), 1);
        assert_eq!(state.on_pass(), None);
    }

    // 配送可能な Object が 1 つも無かった Writer は reset で終端する
    //
    // 一度も write_object を呼ばなかった場合と、省略だけがあった場合のどちらもこの分岐になる
    #[test]
    fn termination_is_reset_when_no_object_passes() {
        let state = state_unfiltered();
        assert_eq!(state.termination(None), SubgroupTermination::Reset);
        let mut skipped = state_unfiltered();
        skipped.on_skip();
        assert_eq!(skipped.termination(None), SubgroupTermination::Reset);
    }

    // 省略が無ければ FIN で終端する
    #[test]
    fn termination_is_fin_when_nothing_is_skipped() {
        let mut state = state_at(3);
        assert_eq!(state.on_pass(), Some(true));
        assert_eq!(state.on_pass(), None);
        assert_eq!(state.termination(Some(loc(7, 3))), SubgroupTermination::Fin);
    }

    // Start Location より前の Object だけを省略した場合は FIN で終端する
    //
    // draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) の FIN 条件のとおり
    #[test]
    fn termination_is_fin_when_only_objects_before_start_are_skipped() {
        let mut state = state_at(3);
        // Object 0 / 1 / 2 は Start Location (Object 3) より前なので省略してよい
        state.on_skip();
        state.on_skip();
        state.on_skip();
        // 最初の Pass は Object 3 なので FIRST_OBJECT は false
        assert_eq!(state.on_pass(), Some(false));
        assert_eq!(state.termination(Some(loc(7, 3))), SubgroupTermination::Fin);
    }

    // Start Location と同じ Location の Object を省略した場合は reset で終端する
    //
    // FIN 条件が除外するのは "smaller than" のみであり Start Location 自身は配送対象である
    #[test]
    fn termination_is_reset_when_object_at_start_is_skipped() {
        let mut state = state_at(5);
        // Object 0 を配送してヘッダを送り、Object 1..5 を省略する
        assert_eq!(state.on_pass(), Some(true));
        for _ in 0..4 {
            state.on_skip();
        }
        // 省略した最後の Object は Start Location (Object 5) の 1 つ前なので FIN
        assert_eq!(state.termination(Some(loc(7, 5))), SubgroupTermination::Fin);
        // Object 5 (= Start Location) を省略すると reset 側になる
        state.on_skip();
        assert_eq!(
            state.termination(Some(loc(7, 5))),
            SubgroupTermination::Reset
        );
    }

    // 配送を開始した後に Start Location 以降の Object を省略した場合は reset で終端する
    //
    // Forward State 0 への更新や Range Filter の不通過で配送されなくなった Object がこれにあたる
    // (この層は省略の理由を区別せず Location だけで判定する)
    #[test]
    fn termination_is_reset_when_object_after_start_is_skipped() {
        let mut state = state_at(3);
        // Object 0 / 1 / 2 を省略し、Object 3 / 4 を配送する
        state.on_skip();
        state.on_skip();
        state.on_skip();
        assert_eq!(state.on_pass(), Some(false));
        assert_eq!(state.on_pass(), None);
        // Object 5 を省略すると reset 側になる
        state.on_skip();
        assert_eq!(
            state.termination(Some(loc(7, 3))),
            SubgroupTermination::Reset
        );
    }

    // 配送した後の省略が Start Location より前なら FIN のままにする
    #[test]
    fn termination_is_fin_when_later_skip_is_before_start() {
        let mut state = state_at(10);
        assert_eq!(state.on_pass(), Some(true));
        state.on_skip();
        assert_eq!(
            state.termination(Some(loc(7, 10))),
            SubgroupTermination::Fin
        );
    }

    // Start Location が Subgroup 中に拡大した場合も、配送対象だった Object の省略は reset 側
    //
    // draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) は Start Location の拡大を
    // reset の例に挙げる。省略した時点では配送対象だったため、終端時点の Start Location が
    // その後ろにあっても reset 側に倒す (開始時点の snapshot の寄与)
    #[test]
    fn termination_is_reset_when_start_location_grows_after_skip() {
        let mut state = state_at(0);
        // Object 0..4 を配送し、Object 5..7 を省略する
        assert_eq!(state.on_pass(), Some(true));
        for _ in 0..4 {
            state.on_pass();
        }
        for _ in 0..3 {
            state.on_skip();
        }
        // 終端時点の Start Location は Object 8 まで拡大しており、省略した Object 5..7 は現在の
        // Start Location より前だが、省略した時点では配送対象 (Object 0 以降) だったため reset 側
        assert_eq!(
            state.termination(Some(loc(7, 8))),
            SubgroupTermination::Reset
        );
    }

    // Subgroup 開始時点で Location Filter が無かった場合も、途中で追加された Start Location より
    // 前の Object の省略は reset 側
    //
    // 省略した時点ではすべての Object が配送対象だった (開始時点の snapshot が `None` の寄与)
    #[test]
    fn termination_is_reset_when_filter_is_added_after_skip() {
        let mut state = state_unfiltered();
        // Object 0..4 を配送し、Object 5 / 6 / 7 を省略する
        assert_eq!(state.on_pass(), Some(true));
        for _ in 0..4 {
            state.on_pass();
        }
        for _ in 0..3 {
            state.on_skip();
        }
        assert_eq!(state.on_pass(), None);
        // 終端時点で Start Location が Object 8 に追加されていても、省略時点では配送対象だった
        assert_eq!(
            state.termination(Some(loc(7, 8))),
            SubgroupTermination::Reset
        );
    }

    // Start Location が Subgroup 中に縮小した場合、縮小前に省略した Object が配送対象に戻るため
    // reset 側
    //
    // draft-ietf-moq-transport-21 §9.5.1 (Updating Subscriptions) は Start Location の縮小を
    // 想定する。FIN を送ると Subgroup が完備と誤認され、欠落分の FETCH が行われなくなる
    // (終端時点の現在値の寄与)
    #[test]
    fn termination_is_reset_when_start_location_shrinks_after_skip() {
        let mut state = state_at(10);
        // Object 0..9 を省略し、Object 10 から配送する
        for _ in 0..10 {
            state.on_skip();
        }
        assert_eq!(state.on_pass(), Some(false));
        // 開始時点の Start Location より前の省略だけだったとしても、終端時点で縮小していれば
        // 省略した Object が配送対象に含まれる
        assert_eq!(
            state.termination(Some(loc(7, 5))),
            SubgroupTermination::Reset
        );
    }

    // 終端時点で Location Filter が削除された場合も、省略した Object はすべて配送対象になる
    #[test]
    fn termination_is_reset_when_filter_is_removed_after_skip() {
        let mut state = state_at(10);
        for _ in 0..10 {
            state.on_skip();
        }
        assert_eq!(state.on_pass(), Some(false));
        assert_eq!(state.termination(None), SubgroupTermination::Reset);
    }

    // 別 Group を Start Location に持つ購読では、Group が小さい Object の省略は FIN のまま
    //
    // Location の比較が (group_id, object_id) の辞書順であることを固定する
    #[test]
    fn termination_is_fin_when_skipped_group_is_before_start() {
        let mut state = state_in_group(5, Some(loc(7, 0)));
        assert_eq!(state.on_pass(), Some(true));
        state.on_skip();
        assert_eq!(state.termination(Some(loc(7, 0))), SubgroupTermination::Fin);
    }

    // FIN の場合は Stream Reset Error Code を持たず、RESET の場合は CANCELLED を持つ
    #[test]
    fn reset_error_code_is_none_for_fin_and_cancelled_for_reset() {
        assert_eq!(SubgroupTermination::Fin.reset_error_code(), None);
        assert_eq!(
            SubgroupTermination::Reset.reset_error_code(),
            Some(DataStreamResetReason::Cancelled.error_code())
        );
    }
}

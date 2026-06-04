//! Subgroup Stream ライター
//!
//! 1 つの Group に対応する一方向ストリームへオブジェクトを書き込む。
//! draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §11.3.2 (Closing Subgroup Streams) に準拠する。

use bytes::Bytes;

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::{
    session::types::DataStreamId, session::types::DataStreamResetReason,
    session::types::RequestStreamEnd, stream::subgroup::SubgroupHeader,
    stream::subgroup::SubgroupIdMode, stream::subgroup::SubgroupObject,
};

use crate::error::Result;
use moqt_example_transport::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use moqt_example_transport::transport;

/// Subgroup の Object ID 追跡とヘッダ送信の状態 (sans I/O)
///
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §2.2 (Subgroups): FIRST_OBJECT bit は
/// 「その subgroup stream の最初の Object が original publisher により subgroup に公開された
/// 最初の Object である」ことを示す。Object 0 がフィルタ不通過でスキップされた場合は false に
/// する必要があるため、最初の Pass までヘッダ送信を遅延して確定させる。
/// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
#[derive(Debug)]
struct SubgroupObjectState {
    /// 次に送信を試みる Object ID
    next_object_id: u64,
    /// 直前の Pass 済み Object ID (デルタ計算用)
    prev_object_id: Option<u64>,
    /// ワイヤへのヘッダ送信を決めたか
    header_sent: bool,
    /// フィルタ不通過で省略した Object があるか
    has_skipped: bool,
}

impl SubgroupObjectState {
    /// 初期状態 (Object ID 0・直前の Pass なし・Skip なし) を返す
    fn new() -> Self {
        Self {
            next_object_id: 0,
            prev_object_id: None,
            header_sent: false,
            has_skipped: false,
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
    fn on_skip(&mut self) {
        self.next_object_id += 1;
        self.has_skipped = true;
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

    /// 終端を FIN ではなく reset にする必要があるか
    ///
    /// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams): FIN は「Start Location 未満の
    /// Object を除き、配送すべき全 Object を配送した」場合のみに限られ、フィルタ不通過で省略した
    /// Object が 1 つでもある場合は MUST reset。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    fn needs_reset(&self) -> bool {
        !self.header_sent || self.has_skipped
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
    /// Object ID 追跡と FIRST_OBJECT bit の決定状態
    object_state: SubgroupObjectState,
}

impl SubgroupWriter {
    /// 新しい Subgroup Stream を開き、Session に登録する
    ///
    /// ワイヤへの SUBGROUP_HEADER 送信は最初の Pass まで遅延し、`first_object` は最初の Pass が
    /// Object 0 かどうかで確定する (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §2.2 (Subgroups))。
    /// Session 登録 (`send_subgroup_header`) は `send_subgroup_object` のフィルタ評価と
    /// `send_data_stream_closed` の前提になるため、`new` の時点で行う。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub async fn new(
        handle: &transport::StreamHandle,
        data_plane: &DataPlaneHandle,
        request_id: u64,
        track_alias: u64,
        group_id: u64,
        publisher_priority: u8,
        has_properties: bool,
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
            object_state: SubgroupObjectState::new(),
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
    /// 一度も Pass しなかった場合、またはフィルタ不通過で省略した Object が 1 つでもある場合は
    /// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams)
    /// の MUST に従い、FIN ではなく reset で終端する (配送すべき Object を配送する前に閉じる場合は
    /// MUST reset。Start Location 未満の Object のみを省略した場合は FIN 側だが、本 example は
    /// フィルタ不通過による省略を reset として統一する)。
    /// draft は SUBGROUP_HEADER + RESET_STREAM_AT を MAY で示すが、example の transport API に
    /// RESET_STREAM_AT が無いためヘッダを送らず RESET_STREAM で代替する。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    pub fn finish(mut self) -> Result<()> {
        if self.object_state.needs_reset() {
            // 購読者側のフィルタ不通過により配送を中止するため CANCELLED を使う
            // (draft-ietf-moq-transport-21 §12.5 (Stream Reset Error Codes))
            let error_code = DataStreamResetReason::Cancelled.error_code();
            self.stream.reset(error_code)?;
            self.data_plane.send_data_stream_closed(
                self.stream_id,
                RequestStreamEnd::Reset {
                    error_code,
                    reliable_size: None,
                },
            )?;
        } else {
            self.stream.finish()?;
            self.data_plane
                .send_data_stream_closed(self.stream_id, RequestStreamEnd::Fin)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::SubgroupObjectState;

    // Object 0 が Pass した場合は FIRST_OBJECT を true で送る
    #[test]
    fn first_object_true_when_object_zero_passes() {
        let mut state = SubgroupObjectState::new();
        assert_eq!(state.current_object_id(), 0);
        assert_eq!(state.current_object_id_delta(), 0);
        assert_eq!(state.on_pass(), Some(true));
        assert_eq!(state.on_pass(), None);
        assert!(!state.needs_reset());
    }

    // Object 0 が Skip されて Object 1 が最初の Pass になった場合は FIRST_OBJECT を false で送る
    #[test]
    fn first_object_false_when_object_zero_is_skipped() {
        let mut state = SubgroupObjectState::new();
        state.on_skip();
        assert_eq!(state.current_object_id(), 1);
        // 最初の Pass は絶対 ID が delta になる
        assert_eq!(state.current_object_id_delta(), 1);
        assert_eq!(state.on_pass(), Some(false));
        assert_eq!(state.on_pass(), None);
        // Object 0 を省略しているため終端は reset 側になる
        assert!(state.needs_reset());
    }

    // 連続して Pass した場合の delta は 0 になる
    #[test]
    fn object_id_delta_is_zero_for_consecutive_passes() {
        let mut state = SubgroupObjectState::new();
        assert_eq!(state.on_pass(), Some(true));
        assert_eq!(state.current_object_id_delta(), 0);
        assert_eq!(state.on_pass(), None);
    }

    // Pass の間に Skip が挟まっても delta は直前の Pass 済み ID 基準になる
    #[test]
    fn object_id_delta_counts_skipped_gap() {
        let mut state = SubgroupObjectState::new();
        assert_eq!(state.on_pass(), Some(true));
        state.on_skip();
        assert_eq!(state.current_object_id(), 2);
        assert_eq!(state.current_object_id_delta(), 1);
        assert_eq!(state.on_pass(), None);
    }

    // Pass の後に Skip があった場合は FIN ではなく reset で終端する必要がある
    #[test]
    fn needs_reset_when_pass_then_skip() {
        let mut state = SubgroupObjectState::new();
        assert_eq!(state.on_pass(), Some(true));
        state.on_skip();
        assert!(state.needs_reset());
    }

    // 一度も write_object を呼ばなかった Writer も reset で終端する必要がある
    #[test]
    fn needs_reset_when_no_object_written() {
        let state = SubgroupObjectState::new();
        assert!(state.needs_reset());
    }

    // 一度も Pass しなかった場合は reset で終端する必要がある
    #[test]
    fn needs_reset_when_no_object_passes() {
        let mut state = SubgroupObjectState::new();
        state.on_skip();
        assert!(state.needs_reset());
    }
}

//! Subgroup Stream ライター
//!
//! 1 つの Group に対応する一方向ストリームへオブジェクトを書き込む。
//! draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) に準拠する。

use bytes::Bytes;

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::{
    session::types::DataStreamId, session::types::RequestStreamEnd,
    stream::subgroup::SubgroupHeader, stream::subgroup::SubgroupIdMode,
    stream::subgroup::SubgroupObject,
};

use crate::error::Result;
use moqt_example_transport::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use moqt_example_transport::transport;

/// Subgroup Stream ライター
///
/// 1 つの Group (= GOP) に対応する一方向ストリームにオブジェクトを書き込む。
/// draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header)
pub struct SubgroupWriter {
    stream: transport::SendStream,
    data_plane: DataPlaneHandle,
    stream_id: DataStreamId,
    /// 次に書き込むオブジェクト ID
    next_object_id: u64,
    /// 直前のオブジェクト ID (デルタ計算用)
    prev_object_id: Option<u64>,
    /// SUBGROUP_HEADER の PROPERTIES ビット
    has_properties: bool,
}

impl SubgroupWriter {
    /// 新しい Subgroup Stream を開き、ヘッダーを書き込む
    pub async fn new(
        handle: &transport::StreamHandle,
        data_plane: &DataPlaneHandle,
        request_id: u64,
        track_alias: u64,
        group_id: u64,
        publisher_priority: u8,
        has_properties: bool,
    ) -> Result<Self> {
        let mut stream = handle.open_send_stream().await?;
        let stream_id = DataStreamId(stream.stream_id());

        // SubgroupHeader を構築する
        // FIRST_OBJECT bit は無条件に立てるが、オブジェクト 0 がフィルタ不通過で
        // スキップされた場合、ワイヤ上の最初のオブジェクトは original publisher が
        // subgroup に公開した最初のオブジェクトではなくなる (draft §11.3.1 (Subgroup Header) の
        // FIRST_OBJECT bit の主張と矛盾する)。ヘッダのワイヤ送信を最初の Pass まで
        // 遅延させれば bit を正しく決められるが、本 example では実装の簡便さのため
        // 先送りする (lib の受信側はこの bit を消費しない)。
        let header = SubgroupHeader {
            track_alias,
            group_id,
            subgroup_id: SubgroupIdMode::Zero,
            publisher_priority: Some(publisher_priority),
            has_properties,
            end_of_group: false,
            first_object: true,
        };

        let encoded = header.encode();
        stream.send(Bytes::from(encoded)).await?;
        data_plane.send_subgroup_header(stream_id, request_id, &header)?;

        Ok(Self {
            stream,
            data_plane: data_plane.clone(),
            stream_id,
            next_object_id: 0,
            prev_object_id: None,
            has_properties,
        })
    }

    /// オブジェクトを書き込む
    ///
    /// LOC プロパティとペイロードを含むオブジェクトをストリームに書き込む。
    /// Session のフィルタ評価を先に行い、通過時のみワイヤへ出す
    /// (draft-ietf-moq-transport-21 §3.3.3 (Combining Filters): "The publisher MUST forward
    /// only objects that pass all filters")。フィルタ不通過
    /// (`SESSION_LOCAL_FILTER_MISMATCH`) のオブジェクトはワイヤに出さず `Skip` を返す。
    /// オブジェクト ID だけ進める (ID ギャップは draft §11.3.1 (Subgroup Header) の
    /// "A consumer cannot infer information about the existence of Objects between the current
    /// and previous Object ID in the Subgroup" により許容される。なおフィルタ由来の
    /// ギャップに PRIOR_OBJECT_ID_GAP (draft §10.9 (Prior Object ID Gap)) は付与しない: 同プロパティは任意であり、
    /// フィルタは REQUEST_UPDATE で変化しうるため確定情報を出せない以上、省略が最も安全)。
    /// `prev_object_id` は実際にワイヤへ送信したオブジェクトでのみ更新する
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
        let object_id = self.next_object_id;

        // object_id_delta を計算する
        let object_id_delta = match self.prev_object_id {
            None => object_id,
            Some(prev) => object_id - prev - 1,
        };

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
            self.next_object_id = object_id + 1;
            return Ok(ObjectFilterOutcome::Skip);
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

        self.prev_object_id = Some(object_id);
        self.next_object_id = object_id + 1;

        Ok(ObjectFilterOutcome::Pass)
    }

    /// ストリームを終了する
    pub fn finish(mut self) -> Result<()> {
        self.stream.finish()?;
        self.data_plane
            .send_data_stream_closed(self.stream_id, RequestStreamEnd::Fin)?;
        Ok(())
    }
}

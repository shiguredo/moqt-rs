//! Object Datagram ライター
//!
//! 1 つの Group に対応する Object Datagram を送信する。
//! draft-ietf-moq-transport-21 §11.2.1 (Object Datagram) に準拠する。

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::stream::datagram::ObjectDatagram;

use crate::error::Result;
use moqt_example_transport::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use moqt_example_transport::transport;

/// Object Datagram ライター
///
/// 1 つの Group (= GOP) に対応する datagram を送信する。
/// draft-ietf-moq-transport-21 §11.2.1 (Object Datagram)
pub struct DatagramWriter {
    handle: transport::StreamHandle,
    data_plane: DataPlaneHandle,
    request_id: u64,
    track_alias: u64,
    group_id: u64,
    publisher_priority: u8,
    /// 次に書き込むオブジェクト ID
    next_object_id: u64,
}

impl DatagramWriter {
    /// 新しい DatagramWriter を作成する
    ///
    /// stream は開かない。datagram 送信ごとにヘッダを生成して送信する。
    pub fn new(
        handle: &transport::StreamHandle,
        data_plane: &DataPlaneHandle,
        request_id: u64,
        track_alias: u64,
        group_id: u64,
        publisher_priority: u8,
    ) -> Self {
        Self {
            handle: handle.clone(),
            data_plane: data_plane.clone(),
            request_id,
            track_alias,
            group_id,
            publisher_priority,
            next_object_id: 0,
        }
    }

    /// オブジェクトを datagram で送信する
    ///
    /// LOC プロパティとペイロードを含むオブジェクトを datagram で送信する。
    /// Session のフィルタ評価を先に行い、通過時のみワイヤへ出す
    /// (draft-ietf-moq-transport-21 §3.3.3 (Combining Filters): "The publisher MUST forward
    /// only objects that pass all filters")。フィルタ不通過
    /// (`SendRequestError::LocalFilterMismatch`) のオブジェクトはワイヤに出さず `Skip` を返す。
    /// オブジェクト ID だけ進める (datagram は絶対 ID のため delta の考慮は不要)。
    ///
    /// 返り値の `ObjectFilterOutcome::Pass` はワイヤへ送信したこと、`Skip` はフィルタ
    /// 不通過で送信しなかったことを示す。呼び出し側は `Skip` をスキップとして扱い、
    /// 送信を継続する (その他のエラーは伝播する)。
    ///
    /// 検証 (手動): OBJECT_PROPERTY_FILTER 付きの subscription に対して、
    /// フィルタ外のオブジェクトが subscriber に届かず publisher が生存すること、
    /// フィルタを通過するオブジェクトの配信が従来どおり行われることを確認する。
    /// リポジトリ内の moqt-subscriber はフィルタ付き SUBSCRIBE を発行する手段を
    /// 持たないため、 subscriber 側にフィルタを付与する改変が必要である。
    pub async fn write_object(
        &mut self,
        payload: &[u8],
        properties: &LocProperties,
    ) -> Result<ObjectFilterOutcome> {
        let object_id = self.next_object_id;

        // LOC プロパティをエンコードする
        let properties_data = if properties.is_empty() {
            None
        } else {
            Some(properties.encode()?)
        };

        let datagram = ObjectDatagram {
            track_alias: self.track_alias,
            group_id: self.group_id,
            object_id,
            publisher_priority: Some(self.publisher_priority),
            properties_data,
            end_of_group: false,
            status: None,
        };

        let header = datagram.encode()?;
        let mut buf = Vec::with_capacity(header.len() + payload.len());
        buf.extend_from_slice(&header);
        buf.extend_from_slice(payload);

        // フィルタ評価は doc のとおりワイヤ送信より先に行う
        let outcome = self.data_plane.send_object_datagram(
            self.request_id,
            self.group_id,
            object_id,
            datagram.properties_data,
            None,
        )?;
        if outcome == ObjectFilterOutcome::Skip {
            tracing::debug!(
                "skip object datagram (filter mismatch): request_id={}, object_id={}",
                self.request_id,
                object_id,
            );
            self.next_object_id = object_id + 1;
            return Ok(ObjectFilterOutcome::Skip);
        }

        self.handle.send_datagram(&buf).await?;

        self.next_object_id = object_id + 1;

        Ok(ObjectFilterOutcome::Pass)
    }
}

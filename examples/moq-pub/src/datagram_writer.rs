//! Object Datagram ライター
//!
//! 1 つの Group に対応する Object Datagram を送信する。
//! draft-ietf-moq-transport-22 §11.2.1 (Object Datagram) に準拠する。

use shiguredo_moqt::loc::LocProperties;
use shiguredo_moqt::stream::datagram::ObjectDatagram;

use crate::error::{Error, Result};
use tokio_moq::moqt_client::{DataPlaneHandle, ObjectFilterOutcome};
use tokio_moq::transport;

/// Object Datagram ライター
///
/// draft-ietf-moq-transport-22 §11.2.1 (Object Datagram) の OBJECT_DATAGRAM を送信する。
/// 上限サイズを超える object は送らずにエラーにする (datagram は 1 つの QUIC パケットに
/// 収まる必要がある)。
pub struct DatagramWriter {
    handle: transport::StreamHandle,
    data_plane: DataPlaneHandle,
    request_id: u64,
    track_alias: u64,
    group_id: u64,
    publisher_priority: u8,
    /// object datagram の上限サイズ (bytes)
    ///
    /// datagram は 1 つの QUIC パケットに収まる必要がある (draft-ietf-moq-loc-04 §4.1)。
    /// 上限を超える object は送らずにエラーにする。
    max_size: usize,
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
        max_size: usize,
    ) -> Self {
        Self {
            handle: handle.clone(),
            data_plane: data_plane.clone(),
            request_id,
            track_alias,
            group_id,
            publisher_priority,
            max_size,
            next_object_id: 0,
        }
    }

    /// オブジェクトを datagram で送信する
    ///
    /// LOC プロパティとペイロードを含むオブジェクトを datagram で送信する。
    /// Session のフィルタ評価を先に行い、通過時のみワイヤへ出す
    /// (draft-ietf-moq-transport-22 §3.3.3 (Combining Filters): "The publisher MUST forward
    /// only objects that pass all filters.")。フィルタ不通過
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
    /// リポジトリ内の moq-sub はフィルタ付き SUBSCRIBE を発行する手段を
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
            datagram.end_of_group,
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

        // RFC 9221 §3 は peer が広告した max_datagram_frame_size を超える DATAGRAM の送信を
        // MUST NOT とし、受信側は PROTOCOL_VIOLATION で接続を閉じる MUST を負う。path MTU を
        // 超えた datagram は s2n-quic が通知なく破棄し、draft-ietf-moq-transport-22 §11.2 も
        // 「上限を超えた object は通知なく破棄される」と定める。送信前にサイズを判定して
        // 原因と対処が分かるエラーにする。
        check_datagram_size(buf.len(), self.max_size)?;

        self.handle.send_datagram(&buf).await?;

        self.next_object_id = object_id + 1;

        Ok(ObjectFilterOutcome::Pass)
    }
}

/// object datagram のサイズが上限内かどうかを判定する
///
/// datagram は分割できず (RFC 9221 §5 "DATAGRAM frames cannot be fragmented")、1 つの QUIC
/// パケットに収まる必要があるため、送信前にこの判定を行う。
fn check_datagram_size(size: usize, max_size: usize) -> Result<()> {
    if size > max_size {
        return Err(Error::DatagramTooLarge { size, max_size });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 上限と等しいサイズは送信でき、上限を超えるとエラーになる
    ///
    /// datagram は 1 つの QUIC パケットに収まる必要があるため (draft-ietf-moq-loc-04 §4.1)、
    /// 境界 (上限未満 / 上限と等しい / 上限 + 1) を固定する。
    #[test]
    fn datagram_size_boundary() {
        let max_size = crate::cli::DEFAULT_DATAGRAM_MAX_SIZE;
        assert!(
            check_datagram_size(max_size - 1, max_size).is_ok(),
            "上限未満のサイズは送信できること"
        );
        assert!(
            check_datagram_size(max_size, max_size).is_ok(),
            "上限と等しいサイズは送信できること"
        );

        let error = check_datagram_size(max_size + 1, max_size)
            .expect_err("上限を超えるとエラーになること");
        assert!(
            matches!(
                error,
                Error::DatagramTooLarge { size, max_size: limit }
                    if size == crate::cli::DEFAULT_DATAGRAM_MAX_SIZE + 1
                        && limit == crate::cli::DEFAULT_DATAGRAM_MAX_SIZE
            ),
            "実サイズと上限が保持されること: {error}"
        );
    }
}

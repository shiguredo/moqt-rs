//! data stream の種別判定ヘルパー
//!
//! `ControlStream` / `StreamRead` は `moqt_example_transport::moqt_client` が提供する。

use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::stream::{DataStreamType, classify_data_stream_type};

use crate::error::Result;
use moqt_example_transport::moqt_client::StreamRead;
use moqt_example_transport::transport;

/// 受信ストリームの種別 (data stream 用)
#[derive(Debug, Clone, Copy)]
pub enum StreamType {
    /// SubgroupHeader で始まるデータストリーム
    Subgroup,
    /// FetchHeader で始まる FETCH 応答ストリーム
    Fetch,
    /// 帯域幅プロービング用のパディングストリーム
    Padding,
}

/// ストリーム先頭の stream type を varint で読んで種別を判定する
///
/// `buf` には読み込んだ全バイトが蓄積され、呼び出し側が後続のデコーダに
/// そのまま `push` して使用できる。
pub async fn peek_stream_type(
    stream: &mut transport::RecvStream,
    buf: &mut Vec<u8>,
) -> Result<StreamRead<(u64, StreamType)>> {
    let mut decoder = MessageDecoder::new();
    loop {
        if let Some(stream_type) = decoder.try_decode_varint()? {
            let stream_kind = match classify_data_stream_type(stream_type) {
                Some(DataStreamType::Fetch) => StreamType::Fetch,
                Some(DataStreamType::Padding) => StreamType::Padding,
                _ => StreamType::Subgroup,
            };
            return Ok(StreamRead::Value((stream_type, stream_kind)));
        }
        match stream.receive_chunk().await? {
            transport::RecvChunk::Data(data) => {
                buf.extend_from_slice(&data);
                decoder.push(&data);
            }
            transport::RecvChunk::End(end) => return Ok(StreamRead::Closed(end)),
        }
    }
}

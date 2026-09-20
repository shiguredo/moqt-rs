//! src/stream の単体テスト
//!
//! 本ファイルには integration test 共有の import と helper を置き、検証は test_stream/ 配下へ
//! 分割する。
use shiguredo_moqt::stream::fetch::{
    FetchPriorContext, FetchStreamEntry, FetchStreamObject, FetchSubgroupIdMode,
};
use shiguredo_moqt::{
    error::MessageError, stream::datagram::ObjectDatagram, stream::decoder::DecodedFetchEntry,
    stream::decoder::DecodedFetchObject, stream::decoder::FetchStreamDecoder,
    stream::decoder::SubgroupStreamDecoder, stream::encoder::FetchObjectInput,
    stream::encoder::FetchStreamEncoder, stream::fetch::FetchHeader,
    stream::subgroup::SubgroupHeader, stream::subgroup::SubgroupIdMode,
    stream::subgroup::SubgroupObject,
};

/// FetchStreamDecoder のバッファからペイロードを読み出して長さを検証する
///
/// encoder 側と decoder 側の両テストで使うため親モジュールに置く。
fn drain_fetch_payload(decoder: &mut FetchStreamDecoder, expected_length: u64) {
    let payload = decoder.try_read_payload().expect("payload が取れる");
    assert_eq!(
        payload.len() as u64,
        expected_length,
        "読み出したペイロード長が期待値と一致すること"
    );
}

/// Fetch テストで期待する Object エントリを作る
///
/// 型付きで比較するために `DecodedFetchEntry` を直接組み立てる。
/// `subgroup_id = 0` / `publisher_priority = 128` / `payload_length = 1` / Properties 無しを
/// 固定するため、これ以外の値を使うフィクスチャでは期待値を別に組み立てること。
fn decoded_fetch_object(group_id: u64, object_id: u64) -> DecodedFetchEntry {
    DecodedFetchEntry::Object(DecodedFetchObject {
        group_id,
        subgroup_id: 0,
        object_id,
        publisher_priority: 128,
        is_datagram_origin: false,
        payload_length: 1,
        properties_bytes: None,
    })
}

#[path = "test_stream/subgroup_header.rs"]
mod subgroup_header;

#[path = "test_stream/fetch_header.rs"]
mod fetch_header;

#[path = "test_stream/subgroup_object.rs"]
mod subgroup_object;

#[path = "test_stream/fetch_stream_object.rs"]
mod fetch_stream_object;

#[path = "test_stream/object_datagram.rs"]
mod object_datagram;

#[path = "test_stream/encoder.rs"]
mod encoder;

#[path = "test_stream/decoder.rs"]
mod decoder;

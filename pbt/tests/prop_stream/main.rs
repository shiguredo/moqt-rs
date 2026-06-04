//! src/stream の各ワイヤ型 (subgroup / datagram / fetch) の encode/decode
//! ラウンドトリップを検証する PBT。
//!
//! 対象型: `SubgroupHeader`, `SubgroupObject`, `ObjectDatagram`, `FetchHeader`,
//! `FetchStreamEntry` (`FetchStreamObject` を含む)。
//!
//! `src/stream/` はディレクトリモジュールのため、AGENTS.md が参照する shiguredo-rust スキル
//! 「ディレクトリモジュールの場合は pbt/tests/prop_<module>/main.rs にサブモジュール対応で
//! 分割」に従い、ソースの責務別ファイル (subgroup.rs / datagram.rs / fetch.rs) に対応する
//! サブモジュールへ分割する。
//!
//! `FetchStreamEncoder` / `FetchStreamDecoder` はストリーミングデコーダであり、単純な
//! ラウンドトリップ PBT とは異なるテスト戦略が必要なため、対象外とする。

mod datagram;
mod decoder;
mod encoder;
mod fetch;
mod subgroup;

/// Properties Length (varint) + データ本体を連結した「Properties 生バイト列」を作る。
///
/// `SUBGROUP_OBJECT` / `FETCH_STREAM_OBJECT` の `properties_data` は、Properties Length を
/// 含む生バイト列を要求する (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §11.4.1 (Fetch Header): 空でも Length=0 を含む)。
/// `ObjectDatagram` の `properties_data` は Length を含まないデータ本体のみを渡す点に注意
/// (こちらはこのヘルパを使わない)。
pub(crate) fn length_prefixed_properties(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(data.len() as u64, &mut buf);
    buf.extend_from_slice(data);
    buf
}

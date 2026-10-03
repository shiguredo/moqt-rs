//! src/stream の各ワイヤ型 (subgroup / datagram / fetch) の encode/decode
//! ラウンドトリップを検証する PBT。
//!
//! 対象型: `SubgroupHeader`, `SubgroupObject`, `ObjectDatagram`, `FetchHeader`,
//! `FetchStreamEntry` (`FetchStreamObject` を含む)。
//!
//! `src/stream/` はディレクトリモジュールのため、AGENTS.md が参照する shiguredo-rust スキル
//! 「ディレクトリモジュールの場合は pbt/tests/prop_<module>/main.rs にサブモジュール対応で
//! 分割」に従い、サブモジュール (subgroup.rs / datagram.rs / fetch.rs / decoder.rs / encoder.rs) に分ける。
//!
//! `FetchStreamDecoder` はチャンク分割の影響を受けないこと (分割不変式) を decoder.rs、
//! `FetchStreamEncoder` は昇順 (0x01) / 降順 (0x02) のラウンドトリップを encoder.rs で扱う。

mod datagram;
mod decoder;
mod encoder;
mod fetch;
mod subgroup;

/// Properties Length (varint) + データ本体を連結した「Properties 生バイト列」を作る。
///
/// `SUBGROUP_OBJECT` / `FETCH_STREAM_OBJECT` / `OBJECT_DATAGRAM` の `properties_data` は、
/// Properties Length を含む生バイト列を要求する
/// (draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header) / §11.4.1 (Fetch Header) /
/// §11.2.1 (Object Datagram): 空でも Length=0 を含む。ただし datagram は Length=0 が禁止)。
pub(crate) fn length_prefixed_properties(data: &[u8]) -> Vec<u8> {
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(data.len() as u64, &mut buf);
    buf.extend_from_slice(data);
    buf
}

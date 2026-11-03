//! FETCH_HEADER / FETCH_STREAM_OBJECT (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header))
//!
//! FETCH_HEADER の型 ID は 0x05。
//! FETCH_STREAM_OBJECT の Serialization Flags は以下の通り:
//! - [Group ID Delta (vi64)]    — bit3 が立っている場合
//! - [Subgroup ID (vi64)]       — bits 0-1 が 0b11 の場合
//! - [Object ID Delta (vi64)]   — bit2 が立っている場合
//! - [Publisher Priority (8)]   — bit4 が立っている場合
//! - [Properties (..)]          — bit5 が立っている場合
//! - Object Payload Length (vi64)
//! - [Object Payload (..)]
//!
//! End of Group (0x80) / End of Track (0x81) は
//! draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields) に従い、他のフィールドを
//! 持たない 1 個の vi64 だけで直前の Object が Group / Track の最終 Object であることを示す。
//!
//! End of Range 指示 (0x8C / 0x10C / 0x20C / 0x40C) は
//! draft-ietf-moq-transport-23 §11.4.2 (End of Range) に従い Group ID Delta と
//! Object ID Delta を運ぶ。FETCH_HEADER 直後の最初の entry だけが絶対 ID で、
//! それ以外は直前の Object または End of Range の Location からのデルタである
//! (§11.4.2.1 (ID Decoding))。
//!
//! Object の Object ID Delta は、Group ID Delta が absent のとき
//! prior Object ID + delta + 2 を表し、present のときは新しい Group の絶対 Object ID を
//! 表す (§11.4.1.1 (Flags))。End of Range の同一 Group の +1 (§11.4.2.1 (ID Decoding))
//! とは別規則である。
use super::{FETCH_HEADER_TYPE, validate_properties_blob};
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// FETCH_HEADER (draft-ietf-moq-transport-22 §11.4.1 (Fetch Header))
#[derive(Debug, Clone, PartialEq)]
pub struct FetchHeader {
    /// 対応する FETCH 要求の Request ID
    pub request_id: u64,
}

impl FetchHeader {
    /// ヘッダ全体をエンコードする
    pub fn encode(&self) -> Vec<u8> {
        let mut buf = Vec::new();
        varint::encode(FETCH_HEADER_TYPE, &mut buf);
        varint::encode(self.request_id, &mut buf);
        buf
    }

    /// バッファ先頭から FETCH_HEADER をデコードし `(header, 消費バイト数)` を返す
    pub fn decode(buf: &[u8]) -> Result<(Self, usize), MessageError> {
        let mut pos = 0;
        let (type_id, n) = varint::decode(&buf[pos..])?;
        pos += n;
        if type_id != FETCH_HEADER_TYPE {
            // draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams): 未知の
            // ストリーム型は INVALID_STREAM_HEADER (0x200) でセッションを閉じる (MUST)。
            return Err(MessageError::InvalidStreamHeader(
                "fetch header type must be 0x05",
            ));
        }
        let (request_id, n) = varint::decode(&buf[pos..])?;
        pos += n;
        Ok((Self { request_id }, pos))
    }
}

/// Serialization Flags の特殊値: End of Non-Existent Range
pub(crate) const FETCH_END_OF_NON_EXISTENT_RANGE: u64 = 0x8C;
/// Serialization Flags の特殊値: End of Unknown Range
pub(crate) const FETCH_END_OF_UNKNOWN_RANGE: u64 = 0x10C;
/// Serialization Flags の特殊値: End of Timed-Out Range (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) Table 8)
pub(crate) const FETCH_END_OF_TIMED_OUT_RANGE: u64 = 0x20C;
/// Serialization Flags の特殊値: End of Gone Range (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) Table 8)
pub(crate) const FETCH_END_OF_GONE_RANGE: u64 = 0x40C;
/// Serialization Flags の特殊値: End of Group (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) Table 8)
///
/// 0x80 は 128 以上であり、通常のフラグ (128 未満) ではなく特殊値として扱う
/// (§11.4.1: "When 128 or greater, the value signals an indicator")。
pub(crate) const FETCH_END_OF_GROUP: u64 = 0x80;
/// Serialization Flags の特殊値: End of Track (draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) Table 8)
pub(crate) const FETCH_END_OF_TRACK: u64 = 0x81;

/// Fetch ストリームの prior 参照文脈 (draft-ietf-moq-transport-23 §11.4.1.1 (Flags) / §11.4.2.1 (ID Decoding))
///
/// encode/decode 時に、prior object を参照するフラグの妥当性を検証し、
/// End of Range の 2 フィールドが絶対 ID かデルタかを決めるために使用する。
/// 呼び出し側がストリーム内のオブジェクト出現状況に応じて適切な値を渡す。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchPriorContext {
    /// ストリームの最初のオブジェクト: すべての prior 参照が禁止
    /// (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))
    ///
    /// End of Range では Group ID Delta / Object ID Delta が絶対 ID を表す
    /// (draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding))。
    First,
    /// End of Range の後で actual object が未出現:
    /// prior Subgroup ID / Priority の参照が禁止
    /// (draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): prior Group ID / Object ID は End of Range の値を使う)
    NoPriorActualObject,
    /// prior 参照がすべて有効
    HasPriorObject,
}

/// Subgroup ID のエンコードモード (Fetch ストリーム内)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchSubgroupIdMode {
    /// 0b00: Subgroup ID は 0
    Zero,
    /// 0b01: 前のオブジェクトと同じ Subgroup ID
    PreviousSame,
    /// 0b10: 前の Subgroup ID + 1
    PreviousPlusOne,
    /// 0b11: Subgroup ID フィールドあり
    Explicit(u64),
}

/// Fetch ストリーム内の Object または範囲指示
///
/// `Object` 以外の variant は、直前の Object が Group / Track の最終 Object であることを
/// 示す marker (`EndOfGroup` / `EndOfTrack`) と、Object が存在しない / 不明 / 期限切れ /
/// Gone である範囲を示す End of Range 指示 (`EndOfNonExistentRange` など) である。
///
/// End of Range は draft-ietf-moq-transport-23 §11.4.2 (End of Range) に従って
/// Group ID Delta と Object ID Delta を運ぶ。Subgroup ID / Priority / Properties は持たない。
/// marker は同 §11.4.2.2 (Prior Object Fields) に従って他のフィールドを持たず、
/// 後続 ID フィールドが存在しない。
///
/// [`FetchStreamEntry::EndOfGoneRange`] は 4 種の End of Range の中で唯一
/// 「Object が存在しないと断定しない」指示である。指す範囲の Object は、元の publisher が
/// 再配送できないだけで存在自体は否定されていない (draft-ietf-moq-transport-23 §2.1.2
/// (Object States) の "The original publisher cannot deliver the Object and will not be able
/// to, but does not know whether it exists.")。Non-Existent は「存在しないと確定」であり、
/// 恒久的で権威的である (同節 "This state is permanent. All signals that an Object does not
/// exist are authoritative.")。元の publisher が state を失った過去 (再起動や保持期間の
/// 経過) を Non-Existent と断定してはならず、Gone として通知する (同節)。
///
/// End of Range の 2 フィールドは wire に現れた生値であり、絶対 Location ではない。
/// [`FetchPriorContext::First`] (FETCH_HEADER 直後の最初の entry) では両フィールドが
/// 絶対 ID を表し、それ以外では直前の Object または End of Range の解決済み Location を
/// 基準にしたデルタを表す (§11.4.2.1 (ID Decoding))。絶対 Location への解決は
/// 高水準の `FetchStreamDecoder` / `FetchStreamEncoder` が担う。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FetchStreamEntry {
    /// 通常のオブジェクト
    Object(FetchStreamObject),
    /// End of Non-Existent Range (0x8C)
    ///
    /// draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と
    /// Object ID Delta が必須で、Subgroup ID / Priority / Properties は存在しない。
    EndOfNonExistentRange {
        /// Group ID Delta (vi64)
        ///
        /// [`FetchPriorContext::First`] では絶対 Group ID になる。
        group_id_delta: u64,
        /// Object ID Delta (vi64)
        ///
        /// [`FetchPriorContext::First`] では絶対 Object ID になる。
        object_id_delta: u64,
    },
    /// End of Unknown Range (0x10C)
    ///
    /// draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と
    /// Object ID Delta が必須で、Subgroup ID / Priority / Properties は存在しない。
    EndOfUnknownRange {
        /// Group ID Delta (vi64)
        ///
        /// [`FetchPriorContext::First`] では絶対 Group ID になる。
        group_id_delta: u64,
        /// Object ID Delta (vi64)
        ///
        /// [`FetchPriorContext::First`] では絶対 Object ID になる。
        object_id_delta: u64,
    },
    /// End of Timed-Out Range (0x20C)
    ///
    /// draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と
    /// Object ID Delta が必須で、Subgroup ID / Priority / Properties は存在しない。
    EndOfTimedOutRange {
        /// Group ID Delta (vi64)
        ///
        /// [`FetchPriorContext::First`] では絶対 Group ID になる。
        group_id_delta: u64,
        /// Object ID Delta (vi64)
        ///
        /// [`FetchPriorContext::First`] では絶対 Object ID になる。
        object_id_delta: u64,
    },
    /// End of Gone Range (0x40C)
    ///
    /// draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と
    /// Object ID Delta が必須で、Subgroup ID / Priority / Properties は存在しない。
    ///
    /// 直前に直列化した Object から、この指示が指す Location までの区間の Object が
    /// Gone であることを示す。Gone は「元の publisher が再配送できないが、存在するか否かは
    /// 知らない」状態であり (draft-ietf-moq-transport-23 §2.1.2 (Object States))、
    /// 存在しないと確定した Non-Existent (`EndOfNonExistentRange`) とは区別する。
    /// 別の情報源が Gone の Object を保持していることがあるため、Gone として記録した
    /// Location の Object を後から受け取っても矛盾ではない (同節 "Because another source can
    /// still hold a Gone Object, receiving an Object that was recorded as Gone or Unknown is
    /// not an error.")。Gone を Non-Existent と同一視すると、この後から届く Object を
    /// §12.1 (Malformed Tracks) 条件 10 の「存在しないと記録した後の Object」として
    /// 誤って拒否してしまう。
    ///
    /// 元の publisher が再起動で state を失った場合や保持期間を過ぎた Object は、
    /// この指示で Gone として通知する (同 §2.1.2 / §3.2.2 (Gaps in a Fetch Stream))。
    /// 上流から Gone の通知を受けていない中継者が Gone を通知してはならない
    /// (§3.2.3 (Relay Fetch Handling) の MUST NOT) が、中継機能は本実装の対象外である。
    EndOfGoneRange {
        /// Group ID Delta (vi64)
        ///
        /// [`FetchPriorContext::First`] では絶対 Group ID になる。
        group_id_delta: u64,
        /// Object ID Delta (vi64)
        ///
        /// [`FetchPriorContext::First`] では絶対 Object ID になる。
        object_id_delta: u64,
    },
    /// End of Group (0x80)
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): "no other fields are
    /// present. The prior Object is the last Object in its Group". marker 自身は
    /// Group ID も Object ID も運ばないため、後続 ID フィールドを持たない。
    /// 直前の Object の Object ID が Group の最終 Object ID になり、それより後ろの
    /// Object は同じ Group に存在しない。
    EndOfGroup,
    /// End of Track (0x81)
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): "The prior Object is the
    /// last Object in ... the Track ... End of Track also indicates End of Group.
    /// The prior Object ID becomes 2^64-1." marker 自身は後続 ID フィールドを持たない。
    EndOfTrack,
}

/// Fetch ストリーム内のオブジェクトヘッダ (draft-ietf-moq-transport-22 §11.4.1 (Fetch Header))
///
/// ペイロードデータ自体は含まない。呼び出し元が `payload_length` バイト分のペイロードを
/// 後続のバッファから読み書きする。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FetchStreamObject {
    /// Group ID Delta (None の場合は前のオブジェクトの Group ID を継承)
    ///
    /// draft-ietf-moq-transport-23 §11.4.1.1 (Flags): 値はデルタエンコードされ、
    /// Ascending では prior Group ID + delta + 1、Descending では prior Group ID - (delta + 1)
    /// になる。最初のオブジェクトでは絶対値として解釈される。
    pub group_id: Option<u64>,
    /// Subgroup ID のエンコードモード (変更なし)
    pub subgroup_id: FetchSubgroupIdMode,
    /// Object ID Delta (None の場合は前の Object ID + 1)
    ///
    /// draft-ietf-moq-transport-23 §11.4.1.1 (Flags): Group ID Delta が absent のときは
    /// prior Object ID + delta + 2 を表し、present のときは新しい Group の絶対 Object ID を
    /// 表す。最初のオブジェクトでは常に絶対値である。この解決は絶対 Location を扱う
    /// [`crate::stream::decoder::FetchStreamDecoder`] /
    /// [`crate::stream::encoder::FetchStreamEncoder`] が担う。
    pub object_id: Option<u64>,
    /// Publisher Priority (None の場合は前のオブジェクトの Priority を継承)
    pub publisher_priority: Option<u8>,
    /// Properties フィールドが存在するか
    pub has_properties: bool,
    /// Datagram 起源のオブジェクトか (draft-ietf-moq-transport-22 §11.4.1.1 (Flags): bit 0x40)
    ///
    /// true の場合、元の Delivery Mode が Datagram であり Subgroup ID を持たない。
    pub is_datagram_origin: bool,
    /// ペイロード長
    pub payload_length: u64,
}

impl FetchStreamEntry {
    /// エントリをエンコードする
    ///
    /// `prior_context` で prior object 参照の妥当性を検証する。
    ///
    /// `properties_data` は Properties Length varint を含む生バイト列である
    /// (draft-ietf-moq-transport-22 §11.1.2 (Object Properties))。
    /// `has_properties` が true の場合に渡し、プロパティが空でも
    /// Properties Length = 0 を含むデータ (`&[0x00]` など) を渡す。
    /// 空スライス (`Some(&[])`) は Properties Length varint を含まない契約違反入力である。
    /// オブジェクト以外の variant (`EndOfNonExistentRange` など) には `None` を渡す
    /// (draft-ietf-moq-transport-23 §11.4.2 (End of Range):
    /// "Subgroup ID, Priority and Properties are not present"。marker も
    /// §11.4.2.2 (Prior Object Fields) の "no other fields are present" により
    /// Properties を持たない)。
    ///
    /// # Errors
    ///
    /// `Object` variant をエンコードするときに、次の条件で `ProtocolViolation` を返す。
    /// 詳細な条件は [`FetchStreamObject::encode`] の doc を参照する。
    ///
    /// - Datagram 起源なのに Subgroup ID を持つ
    /// - `has_properties` と `properties_data` の組み合わせが不正
    ///   (true なのに `None` / 空スライス、または false なのに `Some`)
    /// - Properties Length varint が不正、または Properties Length と実データ長が一致しない
    /// - `prior_context` に対して prior 参照が不正
    ///
    /// オブジェクト以外の variant では、`properties_data` に `Some(...)` を渡すと
    /// `ProtocolViolation` を返す (書き込みは行わない)。`None` なら常に `Ok(())` を返す。
    ///
    /// End of Range の variant が持つ 2 フィールドは生値であり、`prior_context` によって
    /// 意味が変わる。[`FetchPriorContext::First`] では絶対 ID、それ以外では直前の Location からの
    /// デルタを表す (§11.4.2.1 (ID Decoding))。この関数は与えられた生値をそのまま書き出すだけで、
    /// デルタの計算や加減算の overflow 検証は行わない。それは絶対 Location を扱う高水準の
    /// `FetchStreamEncoder` の責務である。
    pub fn encode(
        &self,
        properties_data: Option<&[u8]>,
        prior_context: FetchPriorContext,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        match self {
            Self::EndOfNonExistentRange {
                group_id_delta,
                object_id_delta,
            } => {
                reject_marker_properties(properties_data)?;
                varint::encode(FETCH_END_OF_NON_EXISTENT_RANGE, buf);
                varint::encode(*group_id_delta, buf);
                varint::encode(*object_id_delta, buf);
                Ok(())
            }
            Self::EndOfUnknownRange {
                group_id_delta,
                object_id_delta,
            } => {
                reject_marker_properties(properties_data)?;
                varint::encode(FETCH_END_OF_UNKNOWN_RANGE, buf);
                varint::encode(*group_id_delta, buf);
                varint::encode(*object_id_delta, buf);
                Ok(())
            }
            Self::EndOfTimedOutRange {
                group_id_delta,
                object_id_delta,
            } => {
                reject_marker_properties(properties_data)?;
                varint::encode(FETCH_END_OF_TIMED_OUT_RANGE, buf);
                varint::encode(*group_id_delta, buf);
                varint::encode(*object_id_delta, buf);
                Ok(())
            }
            Self::EndOfGoneRange {
                group_id_delta,
                object_id_delta,
            } => {
                reject_marker_properties(properties_data)?;
                varint::encode(FETCH_END_OF_GONE_RANGE, buf);
                varint::encode(*group_id_delta, buf);
                varint::encode(*object_id_delta, buf);
                Ok(())
            }
            // draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields):
            // "no other fields are present"。marker の vi64 だけを書き出す。
            Self::EndOfGroup => {
                reject_marker_properties(properties_data)?;
                varint::encode(FETCH_END_OF_GROUP, buf);
                Ok(())
            }
            Self::EndOfTrack => {
                reject_marker_properties(properties_data)?;
                varint::encode(FETCH_END_OF_TRACK, buf);
                Ok(())
            }
            Self::Object(obj) => obj.encode(properties_data, prior_context, buf),
        }
    }

    /// バッファ先頭からエントリをデコードし `(entry, 消費バイト数)` を返す
    ///
    /// `prior_context` で prior object 参照の妥当性を検証する。
    /// - `First`: すべての prior 参照を禁止 (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))
    /// - `NoPriorActualObject`: prior Subgroup ID / Priority を禁止 (draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields))
    /// - `HasPriorObject`: すべての prior 参照を許可
    ///
    /// End of Range の variant は wire の生値をそのまま返し、`prior_context` によって
    /// その意味が変わる。`First` では絶対 ID、それ以外では直前の Location からのデルタである
    /// (§11.4.2.1 (ID Decoding))。絶対 Location への解決と overflow 検証は
    /// 高水準の `FetchStreamDecoder` が行う。
    ///
    /// `EndOfGroup` / `EndOfTrack` は後続フィールドを持たないため、`prior_context` に
    /// よらず同じ 1 個の vi64 を読むだけで確定する (§11.4.2.2 (Prior Object Fields))。
    /// marker は prior Object を参照するフィールドを持たないので、`First` 文脈でも
    /// 妥当性検証の対象にならない。
    pub fn decode(
        buf: &[u8],
        prior_context: FetchPriorContext,
    ) -> Result<(Self, usize), MessageError> {
        let (entry, _, consumed) = Self::decode_with_properties(buf, prior_context)?;
        Ok((entry, consumed))
    }

    pub(crate) fn decode_with_properties(
        buf: &[u8],
        prior_context: FetchPriorContext,
    ) -> Result<(Self, Option<Vec<u8>>, usize), MessageError> {
        let mut pos = 0;
        let (flags, n) = varint::decode(&buf[pos..])?;
        pos += n;

        match flags {
            FETCH_END_OF_NON_EXISTENT_RANGE => {
                // draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と
                // Object ID Delta が常に存在する。先頭 entry かどうかで意味が変わるため、
                // ここでは生値のまま返す (§11.4.2.1 (ID Decoding))。
                let (group_id_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                let (object_id_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Ok((
                    Self::EndOfNonExistentRange {
                        group_id_delta,
                        object_id_delta,
                    },
                    None,
                    pos,
                ))
            }
            FETCH_END_OF_UNKNOWN_RANGE => {
                // draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と
                // Object ID Delta が常に存在する。
                let (group_id_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                let (object_id_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Ok((
                    Self::EndOfUnknownRange {
                        group_id_delta,
                        object_id_delta,
                    },
                    None,
                    pos,
                ))
            }
            FETCH_END_OF_TIMED_OUT_RANGE => {
                // draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と
                // Object ID Delta が常に存在する。
                let (group_id_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                let (object_id_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Ok((
                    Self::EndOfTimedOutRange {
                        group_id_delta,
                        object_id_delta,
                    },
                    None,
                    pos,
                ))
            }
            FETCH_END_OF_GONE_RANGE => {
                // draft-ietf-moq-transport-23 §11.4.2 (End of Range): Group ID Delta と
                // Object ID Delta が常に存在する。
                let (group_id_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                let (object_id_delta, n) = varint::decode(&buf[pos..])?;
                pos += n;
                Ok((
                    Self::EndOfGoneRange {
                        group_id_delta,
                        object_id_delta,
                    },
                    None,
                    pos,
                ))
            }
            // draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields):
            // "no other fields are present"。marker の vi64 だけを読んで終わる。
            // 後続 ID フィールドが無いため、分割受信では vi64 が揃った時点で entry が確定する。
            FETCH_END_OF_GROUP => Ok((Self::EndOfGroup, None, pos)),
            FETCH_END_OF_TRACK => Ok((Self::EndOfTrack, None, pos)),
            _ => {
                // Table 8 の特殊値以外の 128 以上は未知値として先に拒否する。
                // prior 文脈違反より未知 flags 自体を優先して報告するのは
                // 実装の診断選択である (いずれもセッション終了であり、
                // draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) は
                // 優先順位を規定しない)。
                if flags >= 128 {
                    // draft-ietf-moq-transport-23 §11.4.1 (Fetch Header): Table 8 に無い
                    // 128 以上の Serialization Flags は MALFORMED_OBJECT (0x201) で閉じる
                    // (MUST)。
                    return Err(MessageError::MalformedObject(
                        "invalid fetch serialization flags",
                    ));
                }
                Self::validate_prior_context(flags, prior_context)?;
                let (obj, properties_bytes, consumed) =
                    FetchStreamObject::decode_after_flags(flags, &buf[pos..])?;
                pos += consumed;
                Ok((Self::Object(obj), properties_bytes, pos))
            }
        }
    }

    /// prior 参照文脈に基づいてフラグの妥当性を検証する
    fn validate_prior_context(
        flags: u64,
        prior_context: FetchPriorContext,
    ) -> Result<(), MessageError> {
        if matches!(prior_context, FetchPriorContext::HasPriorObject) {
            return Ok(());
        }

        let is_datagram_origin = flags & 0x40 != 0;
        // bit 0x40 が立っている場合、下位 2 bit は無視する
        let subgroup_id_mode = if is_datagram_origin {
            0u8
        } else {
            (flags & 0x03) as u8
        };
        let has_priority = flags & 0x10 != 0;

        // prior Subgroup ID 参照の検証 (First / NoPriorActualObject 共通)
        // subgroup_id mode 1 (PreviousSame) または 2 (PreviousPlusOne) は prior 参照
        if subgroup_id_mode == 0x01 || subgroup_id_mode == 0x02 {
            return Err(MessageError::MalformedObject(
                "fetch object must not use prior-relative subgroup ID mode without a prior actual object",
            ));
        }
        // prior Priority 参照の検証 (First / NoPriorActualObject 共通)
        // priority 未指定は prior Priority を意味する
        if !has_priority {
            return Err(MessageError::MalformedObject(
                "fetch object must include explicit Publisher Priority without a prior actual object",
            ));
        }

        // First ではさらに Group ID / Object ID の prior 参照も禁止
        if matches!(prior_context, FetchPriorContext::First) {
            let has_object_id = flags & 0x04 != 0;
            let has_group_id = flags & 0x08 != 0;

            // object_id 未指定は prior Object ID + 1 を意味する
            if !has_object_id {
                return Err(MessageError::MalformedObject(
                    "first fetch object must include explicit Object ID",
                ));
            }
            // group_id 未指定は prior Group ID を意味する
            if !has_group_id {
                return Err(MessageError::MalformedObject(
                    "first fetch object must include explicit Group ID",
                ));
            }
        }

        Ok(())
    }
}

impl FetchStreamObject {
    /// オブジェクトヘッダをエンコードする
    ///
    /// `prior_context` で prior object 参照の妥当性を検証する。
    /// `properties_data` は `has_properties` が true の場合に渡す。
    /// プロパティが空でも `has_properties` が true なら Properties Length = 0 を含むデータを渡す必要がある。
    ///
    /// # Errors
    ///
    /// - Datagram 起源なのに Subgroup ID を持つ: `ProtocolViolation`
    /// - `has_properties` と `properties_data` の組み合わせが不正 (true なのに `None` / 空スライス、
    ///   または false なのに `Some`): `ProtocolViolation`
    /// - Properties Length varint が不正、または Properties Length と実データ長が一致しない: `ProtocolViolation`
    /// - `prior_context` に対して prior 参照が不正: `ProtocolViolation`
    pub fn encode(
        &self,
        properties_data: Option<&[u8]>,
        prior_context: FetchPriorContext,
        buf: &mut Vec<u8>,
    ) -> Result<(), MessageError> {
        // draft-ietf-moq-transport-22 §11.4.1.1 (Flags): Datagram 起源のオブジェクトは Subgroup ID を持たない
        if self.is_datagram_origin && !matches!(self.subgroup_id, FetchSubgroupIdMode::Zero) {
            return Err(MessageError::ProtocolViolation(
                "datagram origin object must not have a Subgroup ID",
            ));
        }

        // draft-ietf-moq-transport-22 §11.4.1.1 (Flags): has_properties と properties_data の整合性を検証する
        // 0x20 bit が立っているときだけ Properties フィールドが present であるべき
        if self.has_properties && properties_data.is_none() {
            return Err(MessageError::ProtocolViolation(
                "has_properties is set but properties_data is not provided",
            ));
        }
        // draft-ietf-moq-transport-22 §11.1.2 (Object Properties): Properties は
        // Properties Length (vi64) + Properties の構造であり、空スライスは Length varint を
        // 含まない契約違反入力。呼び出し側は Length = 0 を含むデータ (`&[0x00]` 等) を渡すこと。
        // この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
        if self.has_properties && properties_data.is_some_and(|data| data.is_empty()) {
            return Err(MessageError::ProtocolViolation(
                "properties data must include Properties Length (empty slice is not allowed)",
            ));
        }
        if !self.has_properties && properties_data.is_some() {
            return Err(MessageError::ProtocolViolation(
                "properties_data is provided but has_properties is not set",
            ));
        }
        // draft-ietf-moq-transport-22 §11.1.2 (Object Properties): Properties Length と
        // 実データ長の一致を書き込み前に検証する
        if let Some(props) = properties_data {
            validate_properties_blob(props)?;
        }

        // prior 参照文脈の検証
        self.validate_encode_prior_context(prior_context)?;

        let mut flags: u64 = 0;

        match &self.subgroup_id {
            FetchSubgroupIdMode::Zero => {}
            FetchSubgroupIdMode::PreviousSame => {
                flags |= 0x01;
            }
            FetchSubgroupIdMode::PreviousPlusOne => {
                flags |= 0x02;
            }
            FetchSubgroupIdMode::Explicit(_) => {
                flags |= 0x03;
            }
        }
        if self.object_id.is_some() {
            flags |= 0x04;
        }
        if self.group_id.is_some() {
            flags |= 0x08;
        }
        if self.publisher_priority.is_some() {
            flags |= 0x10;
        }
        if self.has_properties {
            flags |= 0x20;
        }
        // draft-ietf-moq-transport-22 §11.4.1.1 (Flags): Datagram 起源のオブジェクトは bit 0x40 を設定し、
        // 下位 2 bit を 0 にする
        if self.is_datagram_origin {
            flags |= 0x40;
            flags &= !0x03;
        }

        varint::encode(flags, buf);

        if let Some(group_id) = self.group_id {
            varint::encode(group_id, buf);
        }
        if let FetchSubgroupIdMode::Explicit(id) = &self.subgroup_id {
            varint::encode(*id, buf);
        }
        if let Some(object_id) = self.object_id {
            varint::encode(object_id, buf);
        }
        if let Some(prio) = self.publisher_priority {
            buf.push(prio);
        }
        if let Some(props) = properties_data {
            buf.extend_from_slice(props);
        }

        varint::encode(self.payload_length, buf);

        Ok(())
    }

    /// encode 時の prior 参照文脈を検証する
    fn validate_encode_prior_context(
        &self,
        prior_context: FetchPriorContext,
    ) -> Result<(), MessageError> {
        if matches!(prior_context, FetchPriorContext::HasPriorObject) {
            return Ok(());
        }

        // prior Subgroup ID 参照の検証 (First / NoPriorActualObject 共通)
        if matches!(
            self.subgroup_id,
            FetchSubgroupIdMode::PreviousSame | FetchSubgroupIdMode::PreviousPlusOne
        ) {
            return Err(MessageError::ProtocolViolation(
                "fetch object must not use prior-relative subgroup ID mode without a prior actual object",
            ));
        }
        // prior Priority 参照の検証 (First / NoPriorActualObject 共通)
        if self.publisher_priority.is_none() {
            return Err(MessageError::ProtocolViolation(
                "fetch object must include explicit Publisher Priority without a prior actual object",
            ));
        }

        // First ではさらに Group ID / Object ID の prior 参照も禁止
        if matches!(prior_context, FetchPriorContext::First) {
            if self.object_id.is_none() {
                return Err(MessageError::ProtocolViolation(
                    "first fetch object must include explicit Object ID",
                ));
            }
            if self.group_id.is_none() {
                return Err(MessageError::ProtocolViolation(
                    "first fetch object must include explicit Group ID",
                ));
            }
        }

        Ok(())
    }

    /// Serialization Flags を既にデコード済みの状態からフィールドをデコードする
    ///
    /// draft-ietf-moq-transport-23 §11.4.1 (Fetch Header) Table 8 / §11.4.1.1 (Flags):
    /// 128 未満の全 bit は定義済み (Table 9 の 0x03 / Table 10 の
    /// 0x04 / 0x08 / 0x10 / 0x20 / 0x40) のため mask 検証は no-op であり、
    /// Table 8 の特殊値 (0x80 / 0x81 / 0x8C / 0x10C / 0x20C / 0x40C) 以外の 128 以上は
    /// MALFORMED_OBJECT とする ("If an endpoint receives any other value greater than 127,
    /// it MUST close the session with MALFORMED_OBJECT.")。
    /// 128 以上の検査は呼び出し側 (`decode_with_properties`) でも行う二重検査で
    /// あり、ここは将来の単独利用に備えた防御である。
    /// この節番号・規則は draft 由来であり将来の draft 改版で変わる可能性がある。
    fn decode_after_flags(
        flags: u64,
        buf: &[u8],
    ) -> Result<(Self, Option<Vec<u8>>, usize), MessageError> {
        // flags が 128 以上は通常のフラグではない (特殊値はこのメソッドに来ない)
        if flags >= 128 {
            // draft-ietf-moq-transport-23 §11.4.1 (Fetch Header): MALFORMED_OBJECT (0x201)。
            return Err(MessageError::MalformedObject(
                "invalid fetch serialization flags",
            ));
        }

        let mut pos = 0;

        // draft-ietf-moq-transport-22 §11.4.1.1 (Flags): bit 0x40 は Datagram 起源を示す
        // bit 0x40 が立っている場合、下位 2 bit は無視する (MUST)
        let is_datagram_origin = flags & 0x40 != 0;
        let subgroup_id_mode = if is_datagram_origin {
            0u8 // 下位 2 bit を無視し、Subgroup ID は Zero として扱う
        } else {
            (flags & 0x03) as u8
        };
        let has_object_id = flags & 0x04 != 0;
        let has_group_id = flags & 0x08 != 0;
        let has_priority = flags & 0x10 != 0;
        let has_properties = flags & 0x20 != 0;

        let group_id = if has_group_id {
            let (id, n) = varint::decode(&buf[pos..])?;
            pos += n;
            Some(id)
        } else {
            None
        };

        let subgroup_id = match subgroup_id_mode {
            0b00 => FetchSubgroupIdMode::Zero,
            0b01 => FetchSubgroupIdMode::PreviousSame,
            0b10 => FetchSubgroupIdMode::PreviousPlusOne,
            0b11 => {
                let (id, n) = varint::decode(&buf[pos..])?;
                pos += n;
                FetchSubgroupIdMode::Explicit(id)
            }
            _ => {
                unreachable!("subgroup_id_mode is a 2-bit value and all four patterns are covered")
            }
        };

        let object_id = if has_object_id {
            let (id, n) = varint::decode(&buf[pos..])?;
            pos += n;
            Some(id)
        } else {
            None
        };

        let publisher_priority = if has_priority {
            if buf[pos..].is_empty() {
                return Err(MessageError::UnexpectedEof);
            }
            let prio = buf[pos];
            pos += 1;
            Some(prio)
        } else {
            None
        };

        // Properties を Properties Length varint 込みの生バイト列として切り出す
        // draft-ietf-moq-transport-22 §11.4.1 (Fetch Header): Fetch object でも Properties Length = 0 は合法
        let properties_bytes = if has_properties {
            let prop_start = pos;
            let (prop_len, n) = varint::decode(&buf[pos..])?;
            pos += n;
            let prop_len = varint::checked_len(prop_len, buf[pos..].len())?;
            pos += prop_len;
            Some(buf[prop_start..pos].to_vec())
        } else {
            None
        };

        let (payload_length, n) = varint::decode(&buf[pos..])?;
        pos += n;

        Ok((
            Self {
                group_id,
                subgroup_id,
                object_id,
                publisher_priority,
                has_properties,
                is_datagram_origin,
                payload_length,
            },
            properties_bytes,
            pos,
        ))
    }
}

/// Object 以外の entry に `properties_data` を渡していないことを検証する
///
/// End of Range は draft-ietf-moq-transport-23 §11.4.2 (End of Range) の
/// "Subgroup ID, Priority and Properties are not present" により Properties を持たず、
/// End of Group / End of Track も §11.4.2.2 (Prior Object Fields) の
/// "no other fields are present" により持たない。したがって `Some(...)` は
/// 呼び出し側の渡し間違いとして `ProtocolViolation` にする。
/// `None` はそのまま受理する。
fn reject_marker_properties(properties_data: Option<&[u8]>) -> Result<(), MessageError> {
    match properties_data {
        None => Ok(()),
        Some(_) => Err(MessageError::ProtocolViolation(
            "properties_data must be None for an entry without fields",
        )),
    }
}

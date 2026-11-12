//! Subgroup / Fetch データストリーム用バッファ付きインクリメンタルデコーダ (sans I/O)
//!
//! QUIC/WebTransport のフレーム境界とストリームヘッダ/オブジェクト境界は一致しないため、
//! 受信バッファに蓄積しながらインクリメンタルにデコードする必要がある。
//!
//! このモジュールは I/O を持たず、バッファ管理・デコード試行・プロトコル状態管理のみを提供する。
//! ストリームからのデータ受信およびペイロードの読み出しは呼び出し側が行う。
use super::fetch::{
    FetchHeader, FetchPriorContext, FetchStreamEntry, FetchStreamObject, FetchSubgroupIdMode,
};
use super::subgroup::{SubgroupHeader, SubgroupIdMode, SubgroupObject};
use crate::{error::MessageError, object_properties::ObjectProperties};
use alloc::vec::Vec;
use hashbrown::HashMap;

/// デコード済み Subgroup オブジェクトの情報
///
/// `SubgroupObject` の生のデルタ値ではなく、絶対値に解決済みの情報を提供する。
/// ペイロードは含まない。呼び出し側が `payload_length` バイト分を消費する。
///
/// 手組みの値を作って `Session::recv_subgroup_object` に渡す場合、`status` が `Some` のとき
/// `payload_length` は 0、`status` が `None` のときは 1 以上でなければならない
/// (draft-ietf-moq-transport-23 §11.1.1 (Object Status): status 付き Object は payload を持たない)。
/// wire 経路では `SubgroupObject::decode` がこの不変条件を強制する。
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedSubgroupObject {
    /// 絶対 Object ID (デルタから解決済み)
    pub object_id: u64,
    /// ペイロード長 (0 の場合は status が存在する)
    pub payload_length: u64,
    /// Object Status (payload_length == 0 の場合のみ)
    pub status: Option<u64>,
    /// Properties の生バイト (Properties Length varint + Properties データ)
    ///
    /// Properties フィールドが無い場合 (draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header) の
    /// Type Flags の PROPERTIES ビット (0x01) が 0) は `None`、
    /// ある場合は wire に現れた Properties Length varint と Properties データをそのまま保持した `Some` になる。
    /// draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers) は非最小形の varint も許すため、
    /// 保持される生バイトは最小形とは限らない (Properties Length = 0 は最小形なら `0x00` の 1 バイト)。
    /// `LocProperties::decode` にそのまま渡せる。
    pub properties_bytes: Option<Vec<u8>>,
}

/// Subgroup データストリームのデコーダー状態
#[derive(Debug, Clone, Copy, PartialEq)]
enum SubgroupDecoderState {
    /// ヘッダー未デコード
    AwaitingHeader,
    /// ヘッダーデコード済み、オブジェクト待ち
    AwaitingObject,
    /// 呼び出し側がペイロードを消費中
    /// `remaining` はまだ消費されていないペイロードバイト数
    ConsumingPayload { remaining: u64 },
}

/// Subgroup データストリーム用バッファ付きインクリメンタルデコーダー (sans I/O)
///
/// `MessageDecoder` と同じ push + try_decode パターンでデータストリームをデコードする。
/// プロトコル状態 (`has_properties` の引き継ぎ、`object_id_delta` → 絶対 `object_id` の変換、
/// `SubgroupIdMode::FirstObjectId` の解決) を内部で管理する。
///
/// ペイロードはデコーダの責務外。`try_decode_object` が返す `payload_length` バイト分を
/// 呼び出し側がバッファから消費した後、`consume_payload` を呼んで次のオブジェクトに進む。
///
/// # 使い方 (検証対象外の疑似コード)
///
/// ```text
/// let mut decoder = SubgroupStreamDecoder::new();
/// decoder.push(&data);
///
/// // 1. ヘッダーをデコードする
/// let header = loop {
///     if let Some(h) = decoder.try_decode_header()? {
///         break h;
///     }
///     decoder.push(&more_data);
/// };
///
/// // 2. オブジェクトを順次デコードする
/// loop {
///     match decoder.try_decode_object()? {
///         Some(obj) => {
///             // payload_length バイト分のペイロードを読み出す
///             // ...
///             decoder.consume_payload(obj.payload_length);
///         }
///         None => {
///             // データ不足、追加データを push する
///             decoder.push(&more_data);
///         }
///     }
/// }
/// ```
pub struct SubgroupStreamDecoder {
    buf: Vec<u8>,
    state: SubgroupDecoderState,
    /// SubgroupHeader の has_properties フラグ
    has_properties: bool,
    /// 前回のオブジェクトの絶対 Object ID
    prev_object_id: Option<u64>,
    /// SubgroupIdMode::FirstObjectId の解決済み subgroup_id
    /// None = まだ未解決 (FirstObjectId モードで最初のオブジェクト未受信)
    resolved_subgroup_id: Option<u64>,
    /// SubgroupIdMode が FirstObjectId かどうか
    is_first_object_id_mode: bool,
}

impl SubgroupStreamDecoder {
    /// 新しいデコーダーを作成する
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            state: SubgroupDecoderState::AwaitingHeader,
            has_properties: false,
            prev_object_id: None,
            resolved_subgroup_id: None,
            is_first_object_id_mode: false,
        }
    }

    /// バッファにデータを追加する
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// バッファから SubgroupHeader のデコードを試みる
    ///
    /// - `Ok(Some(header))` — デコード成功、バッファから消費済み。内部状態を初期化済み
    /// - `Ok(None)` — データ不足、追加データが必要
    /// - `Err(e)` — デコードエラー
    ///
    /// ヘッダーが既にデコード済みの場合は `ProtocolViolation` を返す。
    pub fn try_decode_header(&mut self) -> Result<Option<SubgroupHeader>, MessageError> {
        if !matches!(self.state, SubgroupDecoderState::AwaitingHeader) {
            return Err(MessageError::ProtocolViolation(
                "subgroup header already decoded",
            ));
        }
        if self.buf.is_empty() {
            return Ok(None);
        }
        match SubgroupHeader::decode(&self.buf) {
            Ok((header, consumed)) => {
                self.buf.drain(..consumed);
                // 内部状態を初期化する
                self.has_properties = header.has_properties;
                self.is_first_object_id_mode =
                    matches!(header.subgroup_id, SubgroupIdMode::FirstObjectId);
                self.resolved_subgroup_id = match &header.subgroup_id {
                    SubgroupIdMode::Zero => Some(0),
                    SubgroupIdMode::FirstObjectId => None, // 最初のオブジェクトで確定
                    SubgroupIdMode::Explicit(id) => Some(*id),
                };
                self.state = SubgroupDecoderState::AwaitingObject;
                Ok(Some(header))
            }
            Err(MessageError::UnexpectedEof) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// バッファから SubgroupObject のデコードを試みる
    ///
    /// - `Ok(Some(obj))` — デコード成功。`object_id` は絶対値に解決済み。
    ///   呼び出し側は `payload_length` バイト分のペイロードをバッファから読み出した後、
    ///   `consume_payload` を呼ぶこと。
    ///   Object Properties は `properties_bytes` に保持される。
    /// - `Ok(None)` — データ不足、追加データが必要
    /// - `Err(e)` — デコードエラー
    pub fn try_decode_object(&mut self) -> Result<Option<DecodedSubgroupObject>, MessageError> {
        if !matches!(self.state, SubgroupDecoderState::AwaitingObject) {
            return Err(MessageError::ProtocolViolation(
                "not ready to decode object (header not decoded or payload not consumed)",
            ));
        }
        if self.buf.is_empty() {
            return Ok(None);
        }
        match SubgroupObject::decode(&self.buf, self.has_properties) {
            Ok((obj, properties_bytes, consumed)) => {
                self.buf.drain(..consumed);

                // object_id_delta → 絶対 object_id の変換
                // draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header): 初回 Object の Object ID は
                //           Object ID Delta フィールド値そのもの (絶対値)。以降は prev + delta + 1
                let object_id = match self.prev_object_id {
                    None => obj.object_id_delta,
                    Some(prev) => prev
                        .checked_add(obj.object_id_delta)
                        .and_then(|v| v.checked_add(1))
                        .ok_or(MessageError::DeltaEncodingOverflow(
                            "subgroup object ID delta overflow",
                        ))?,
                };
                self.prev_object_id = Some(object_id);

                // SubgroupIdMode::FirstObjectId の解決
                if self.is_first_object_id_mode && self.resolved_subgroup_id.is_none() {
                    self.resolved_subgroup_id = Some(object_id);
                }

                // 不変条件: status.is_some() ⇔ payload_length == 0
                // 根拠: draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header)
                // "The Object Status field is only sent if the Object Payload Length is zero"
                // および §11.1.1 (Object Status)
                // "An Object MUST have an empty payload unless its Object Status value is
                // registered as permitting a payload in the Object Status registry (Section 16.9)."
                // そのため payload_length をそのまま基準にすればよく、status の有無で
                // 分岐する必要はない。
                let payload_length = obj.payload_length;

                if payload_length > 0 {
                    self.state = SubgroupDecoderState::ConsumingPayload {
                        remaining: payload_length,
                    };
                }
                // payload_length == 0 の場合は AwaitingObject のままで次のオブジェクトに進める

                Ok(Some(DecodedSubgroupObject {
                    object_id,
                    payload_length,
                    status: obj.status,
                    properties_bytes,
                }))
            }
            Err(MessageError::UnexpectedEof) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// バッファからペイロードの読み出しを試みる
    ///
    /// バッファに `payload_length` バイト分のデータが揃っていれば、
    /// ペイロードを返して状態を AwaitingObject に遷移する。
    /// データが不足している場合は `None` を返す（追加データを push して再試行する）。
    ///
    /// - `Some(payload)` — ペイロード読み出し成功、次のオブジェクトに進める
    /// - `None` — データ不足、追加データが必要
    pub fn try_read_payload(&mut self) -> Option<Vec<u8>> {
        match self.state {
            SubgroupDecoderState::ConsumingPayload { remaining } => {
                // remaining は wire 由来の payload_length (u64)。32bit 環境で as usize の
                // 切り捨てによる誤読を防ぐため、バッファ長と u64 空間で比較してから変換する。
                // データ不足時は None を返して追加データを待つ意味論のため checked_len は使わない
                if (self.buf.len() as u64) >= remaining {
                    let len = remaining as usize;
                    let payload: Vec<u8> = self.buf.drain(..len).collect();
                    self.state = SubgroupDecoderState::AwaitingObject;
                    Some(payload)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// ペイロードの消費を通知する
    ///
    /// `try_decode_object` が返した `payload_length` バイト分のペイロードを
    /// 呼び出し側が外部で読み出した場合に呼ぶ。
    /// バッファ内のペイロードを読み出す場合は `try_read_payload` を使う。
    ///
    /// `length` は消費したバイト数。
    /// `payload_length` と一致しない、または `ConsumingPayload` 状態以外で
    /// 呼ばれた場合は `Err(ProtocolViolation)` を返し、状態は変わらない。
    pub fn consume_payload(&mut self, length: u64) -> Result<(), MessageError> {
        match self.state {
            SubgroupDecoderState::ConsumingPayload { remaining } => {
                if length != remaining {
                    return Err(MessageError::ProtocolViolation(
                        "consumed payload length does not match remaining",
                    ));
                }
                self.state = SubgroupDecoderState::AwaitingObject;
                Ok(())
            }
            _ => Err(MessageError::ProtocolViolation(
                "consume_payload called in unexpected state",
            )),
        }
    }

    /// `SubgroupIdMode::FirstObjectId` で解決された subgroup_id を返す
    ///
    /// - `SubgroupIdMode::Zero` → `Some(0)`
    /// - `SubgroupIdMode::Explicit(id)` → `Some(id)`
    /// - `SubgroupIdMode::FirstObjectId` → 最初のオブジェクト受信後は `Some(object_id)`、未受信は `None`
    pub fn resolved_subgroup_id(&self) -> Option<u64> {
        self.resolved_subgroup_id
    }

    /// これ以上データが来ないことを通知し、stream 境界として完結しているか検証する
    ///
    /// EOF 時点で header 未受信、partial object header、または未消費ペイロードが
    /// 残っている場合は `UnexpectedEof` を返す。
    ///
    /// ヘッダーのみ + FIN（オブジェクト 0 個）は正規の空 Subgroup として成功を返す。
    /// draft-ietf-moq-transport-23 §11.3.2 (Closing Subgroup Streams) 冒頭の
    /// "If a sender has delivered all objects in a Subgroup to the QUIC stream, except any
    /// Objects with Locations smaller than the subscription's Start Location, it MUST close
    /// the stream with a FIN." により、Subgroup の全オブジェクトが配達対象外の場合に
    /// ヘッダーのみ送って FIN で閉じるのが MUST に沿うフローになる。
    /// 受信側は FIN をこの Subgroup で受信すべき全オブジェクトを受信したことの保証として
    /// 扱う（同節の "An MOQT implementation that processes a stream FIN is assured it has
    /// received all objects in a subgroup from the start of the subscription."）。
    /// 空 Subgroup の通知として同節後半は RESET_STREAM_AT を MAY で規定するが、排他ではなく、
    /// また RESET_STREAM_AT 経路は FIN で終わらないため本関数には到達しない。送信側が
    /// フィルタ等によりオブジェクトを 1 つも送らない場合（MUST reset 節の "Omitting a
    /// Subgroup Object because the subscription is paused" に該当）でも、受信側はワイヤから
    /// 送信側の意図を判定できないため、FIN 終了を正規の完了として受理する。
    /// draft §9.9 (PUBLISH_DONE) の Stream Count も "including streams that contained no
    /// Objects (e.g., an empty Subgroup)" と空 Subgroup の発生を想定する。
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn finish(&self) -> Result<(), MessageError> {
        match self.state {
            SubgroupDecoderState::AwaitingHeader => Err(MessageError::UnexpectedEof),
            SubgroupDecoderState::AwaitingObject => {
                // 状態が AwaitingObject である時点でヘッダーはデコード済み。
                // バッファが空ならヘッダーのみの空 Subgroup も含めて完結とみなす
                // （partial object header はバッファ非空として検出される）。
                if self.buf.is_empty() {
                    Ok(())
                } else {
                    Err(MessageError::UnexpectedEof)
                }
            }
            SubgroupDecoderState::ConsumingPayload { .. } => Err(MessageError::UnexpectedEof),
        }
    }
}

impl Default for SubgroupStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

/// デコード済み Fetch エントリの情報
///
/// `FetchStreamEntry` の生のデルタ値ではなく、絶対値に解決済みの情報を提供する。
/// End of Range の Group ID / Object ID は、
/// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding) に従って直前の Object または
/// End of Range の Location から解決した絶対 Location である。
/// End of Group / End of Track は marker 自身が ID を運ばないため、直前の Object または
/// End of Range の解決済み Group ID だけを提供する (§11.4.2.2 (Prior Object Fields))。
/// ペイロードは含まない。呼び出し側が `payload_length` バイト分を消費する。
#[derive(Debug, Clone, PartialEq)]
pub enum DecodedFetchEntry {
    /// 通常のオブジェクト（デルタ解決済み）
    Object(DecodedFetchObject),
    /// End of Non-Existent Range (0x8C)
    EndOfNonExistentRange {
        /// 終端 Group ID (絶対値)
        group_id: u64,
        /// 終端 Object ID (絶対値)
        object_id: u64,
    },
    /// End of Unknown Range (0x10C)
    EndOfUnknownRange {
        /// 終端 Group ID (絶対値)
        group_id: u64,
        /// 終端 Object ID (絶対値)
        object_id: u64,
    },
    /// End of Timed-Out Range (0x20C, draft-ietf-moq-transport-23 §11.4.1 Table 8)
    EndOfTimedOutRange {
        /// 終端 Group ID (絶対値)
        group_id: u64,
        /// 終端 Object ID (絶対値)
        object_id: u64,
    },
    /// End of Gone Range (0x40C, draft-ietf-moq-transport-23 §11.4.1 Table 8)
    ///
    /// 範囲内の Object は、元の publisher が再配送できないが存在するか否かは分かっていない
    /// (draft-ietf-moq-transport-23 §2.1.2 (Object States))。存在しないと確定した
    /// [`DecodedFetchEntry::EndOfNonExistentRange`] とは区別しなければならない。
    /// Gone の範囲に含まれる Location の Object を後から別経路で受け取っても、
    /// 不存在の記録と矛盾するわけではない (同節 "Because another source can still hold a
    /// Gone Object, receiving an Object that was recorded as Gone or Unknown is not an
    /// error.")。アプリはこの 2 つを混同せず、Non-Existent だけを恒久的な不存在として
    /// 扱うこと (§12.1 (Malformed Tracks) 条件 10 は「不存在と記録した後の Object の受信」と
    /// 「受信済みの Object の不存在通知」の双方向を malformed とするが、Gone は不存在の記録でも
    /// 通知でもない)。
    EndOfGoneRange {
        /// 終端 Group ID (絶対値)
        group_id: u64,
        /// 終端 Object ID (絶対値)
        object_id: u64,
    },
    /// End of Group (0x80)
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): 直前の Object が
    /// その Group の最終 Object であることを示す。marker は ID フィールドを持たず、
    /// デコーダは直前の Object または End of Range の Group ID をそのまま引き継ぐ。
    /// prior Object ID は `2^64 - 1` になるが、これは実在する Object ではなく
    /// 「これより大きい Object ID は存在しない」ことを表す sentinel であるため、
    /// この variant は Object ID を持たない。
    EndOfGroup {
        /// 終端した Group の Group ID (直前の Object または End of Range の解決済み値)
        group_id: u64,
    },
    /// End of Track (0x81)
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): 直前の Object が
    /// Track の最終 Object であることを示す。End of Track は End of Group も意味する。
    /// prior Object ID が `2^64 - 1` になる点は [`DecodedFetchEntry::EndOfGroup`] と同じ。
    EndOfTrack {
        /// 終端した Track の最後の Group ID (直前の Object または End of Range の解決済み値)
        group_id: u64,
    },
}

/// デコード済み Fetch オブジェクトの情報（デルタ解決済み）
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedFetchObject {
    /// 絶対 Group ID
    pub group_id: u64,
    /// 絶対 Subgroup ID
    pub subgroup_id: u64,
    /// 絶対 Object ID
    pub object_id: u64,
    /// Publisher Priority
    pub publisher_priority: u8,
    /// Datagram 起源のオブジェクトか
    pub is_datagram_origin: bool,
    /// ペイロード長
    pub payload_length: u64,
    /// Object Properties の生バイト列 (Properties Length varint + Properties データ)
    ///
    /// Subgroup 経路 (`DecodedSubgroupObject::properties_bytes`) と同じ表現であり、
    /// `LocProperties::decode` にそのまま渡せる。Flags (draft-ietf-moq-transport-23
    /// §11.4.1.1 Table 10) の bit `0x20` が 0 のときは `None`、1 のときは `Some` になる。
    /// §8.1 (Variable-Length Integers) は非最小形の varint も許すため、保持される生バイトは
    /// 最小形とは限らない (Properties Length = 0 は最小形なら `0x00` の 1 バイト)。
    pub properties_bytes: Option<Vec<u8>>,
}

/// FetchStreamDecoder のデコーダー状態
#[derive(Debug, Clone, Copy, PartialEq)]
enum FetchDecoderState {
    /// ヘッダー未デコード
    AwaitingHeader,
    /// ヘッダーデコード済み、エントリ待ち
    AwaitingEntry,
    /// 呼び出し側がペイロードを消費中
    ConsumingPayload { remaining: u64 },
}

/// prior 参照を解決するための直前状態
///
/// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): prior Group ID / Object ID は
/// 直前の Object または End of Range の解決済み Location であり、prior Subgroup ID /
/// Priority は End of Range を挟んでも直前の actual Object の値である。
/// actual Object がまだ出現していない場合、Subgroup ID / Priority は `None` になり、
/// それらを参照する Object は MALFORMED_OBJECT になる。
///
/// End of Group / End of Track は marker 自身が ID を運ばないため、`group_id` を保持したまま
/// `object_id` だけを `2^64 - 1` にする (§11.4.2.2 (Prior Object Fields): "The prior Object ID
/// becomes 2^64-1.")。`2^64 - 1` は実在する Object ではなく、同じ Group の次の Object ID が
/// 表現できないことを表す sentinel であり、後続 Object がこの sentinel を参照すると
/// DELTA_ENCODING_OVERFLOW (0x400) になる (draft-ietf-moq-transport-23 §12.2
/// (Session Termination Codes): "When more than one code applies, an endpoint uses
/// DELTA_ENCODING_OVERFLOW or FIELD_LENGTH_EXCEEDED in preference to the other codes.")。
#[derive(Debug, Clone, Copy)]
struct FetchPriorState {
    group_id: u64,
    object_id: u64,
    /// 直前の actual Object の Subgroup ID (actual Object 未出現なら `None`)
    subgroup_id: Option<u64>,
    /// 直前の actual Object の Publisher Priority (actual Object 未出現なら `None`)
    publisher_priority: Option<u8>,
}

#[derive(Debug, Clone, Copy)]
struct FetchActualObjectState {
    group_id: u64,
    object_id: u64,
}

#[derive(Debug, Clone, Default)]
struct FetchValidationState {
    last_object: Option<FetchActualObjectState>,
    subgroup_priorities: HashMap<(u64, u64), u8>,
    subgroup_max_objects: HashMap<(u64, u64), u64>,
    subgroup_final_objects: HashMap<(u64, u64), u64>,
}

const FETCH_GROUP_ORDER_ASCENDING: u8 = 0x01;
const FETCH_GROUP_ORDER_DESCENDING: u8 = 0x02;

/// Fetch レスポンスストリーム用バッファ付きインクリメンタルデコーダー (sans I/O)
///
/// `FetchPriorContext` の状態遷移を内部で自動管理し、
/// デルタ圧縮された group_id / subgroup_id / object_id を絶対値に解決する。
/// End of Range の Group ID Delta / Object ID Delta も絶対 Location に解決する
/// (draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding))。低水準 codec は wire の生値を
/// 扱うのに対し、このモジュールは常に絶対 Location を扱う。
/// End of Group / End of Track は ID フィールドを持たないため、直前の解決済み Group ID を
/// 引き継ぎ、prior Object ID を `2^64 - 1` にする (§11.4.2.2 (Prior Object Fields))。
///
/// ペイロードはデコーダの責務外。`try_decode_entry` が返す `payload_length` バイト分を
/// 呼び出し側がバッファから消費した後、`consume_payload` を呼んで次のエントリに進む。
///
/// # 使い方 (検証対象外の疑似コード)
///
/// ```text
/// let mut decoder = FetchStreamDecoder::new();
/// decoder.push(&data);
///
/// // 1. ヘッダーをデコードする
/// let header = loop {
///     if let Some(h) = decoder.try_decode_header()? {
///         break h;
///     }
///     decoder.push(&more_data);
/// };
///
/// // 2. エントリを順次デコードする
/// loop {
///     match decoder.try_decode_entry()? {
///         Some(DecodedFetchEntry::Object(obj)) => {
///             // payload_length バイト分のペイロードを読み出す
///             // ...
///             decoder.consume_payload(obj.payload_length);
///         }
///         Some(_) => { /* EndOfRange / EndOfGroup / EndOfTrack エントリ */ }
///         None => {
///             decoder.push(&more_data);
///         }
///     }
/// }
/// ```
pub struct FetchStreamDecoder {
    buf: Vec<u8>,
    state: FetchDecoderState,
    /// FETCH response の Group 順序 (draft-ietf-moq-transport-23 §3.2.1 (Fetch Object Delivery) / §9.19.8 (GROUP ORDER Parameter))
    group_order: u8,
    /// FetchPriorContext の現在の状態
    prior_context: FetchPriorContext,
    /// 直前の解決済み Location と prior 参照用フィールド (デルタ解決用)
    ///
    /// Object だけでなく End of Range も Group ID / Object ID を更新する
    /// (draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding))。Subgroup ID と Publisher
    /// Priority は直前の actual Object の値を保持する (§11.4.2.2 (Prior Object Fields))。
    prior_state: Option<FetchPriorState>,
    /// End of Track を受信済みか
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields) の End of Track は
    /// Track の最終 Object を宣言する。同 §12.1 (Malformed Tracks) 条件 5 は
    /// "An Object is received whose Group and Object ID are larger than the final Object in
    /// the Track." を malformed とするため、宣言後の Object と End of Range を拒否する。
    /// End of Track は End of Group も意味するため、marker の重複は受理する。
    track_ended: bool,
    /// malformed track 条件の検証状態
    validation: FetchValidationState,
}

impl FetchStreamDecoder {
    /// 新しいデコーダーを作成する
    pub fn new() -> Self {
        Self::with_group_order(FETCH_GROUP_ORDER_ASCENDING)
    }

    /// Group Order を指定して新しいデコーダーを作成する
    pub fn new_with_group_order(group_order: u8) -> Result<Self, MessageError> {
        validate_fetch_group_order(group_order)?;
        Ok(Self::with_group_order(group_order))
    }

    fn with_group_order(group_order: u8) -> Self {
        Self {
            buf: Vec::new(),
            state: FetchDecoderState::AwaitingHeader,
            group_order,
            prior_context: FetchPriorContext::First,
            prior_state: None,
            track_ended: false,
            validation: FetchValidationState::default(),
        }
    }

    /// 特定 subgroup の final object を登録する
    ///
    /// FETCH wire format 自体は subgroup の終端を直接伝えないため、呼び出し側が
    /// 既に知っている final object (たとえば cache や Subgroup stream の FIN 由来)
    /// を与えるための hook。
    pub fn set_subgroup_final_object(
        &mut self,
        group_id: u64,
        subgroup_id: u64,
        final_object_id: u64,
    ) -> Result<(), MessageError> {
        let key = (group_id, subgroup_id);
        if let Some(max_seen) = self.validation.subgroup_max_objects.get(&key)
            && *max_seen > final_object_id
        {
            return Err(MessageError::MalformedTrack(
                "object ID exceeds the known final object for the subgroup",
            ));
        }
        self.validation
            .subgroup_final_objects
            .insert(key, final_object_id);
        Ok(())
    }

    /// group 境界での prune: 過去 group の per-group 検証状態をクリアする
    ///
    /// draft-ietf-moq-transport-23 §3.2.1 (Fetch Object Delivery):
    /// "A publisher MUST send fetched groups in the requested group order (see Section 9.19.8)."
    /// group は要求順で送られるため過去 group が再出現することはなく、
    /// per-group エントリを安全に削除できる。
    fn prune_past_group_state(&mut self, ascending: bool, current_group: u64) {
        if ascending {
            // current_group 未満の group を削除
            self.validation
                .subgroup_priorities
                .retain(|&(g, _), _| g >= current_group);
            self.validation
                .subgroup_max_objects
                .retain(|&(g, _), _| g >= current_group);
            self.validation
                .subgroup_final_objects
                .retain(|&(g, _), _| g >= current_group);
        } else {
            // current_group 超過の group を削除
            self.validation
                .subgroup_priorities
                .retain(|&(g, _), _| g <= current_group);
            self.validation
                .subgroup_max_objects
                .retain(|&(g, _), _| g <= current_group);
            self.validation
                .subgroup_final_objects
                .retain(|&(g, _), _| g <= current_group);
        }
    }

    /// バッファにデータを追加する
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// バッファから FetchHeader のデコードを試みる
    ///
    /// - `Ok(Some(header))` — デコード成功、バッファから消費済み
    /// - `Ok(None)` — データ不足、追加データが必要
    /// - `Err(e)` — デコードエラー
    pub fn try_decode_header(&mut self) -> Result<Option<FetchHeader>, MessageError> {
        if !matches!(self.state, FetchDecoderState::AwaitingHeader) {
            return Err(MessageError::ProtocolViolation(
                "fetch header already decoded",
            ));
        }
        if self.buf.is_empty() {
            return Ok(None);
        }
        match FetchHeader::decode(&self.buf) {
            Ok((header, consumed)) => {
                self.buf.drain(..consumed);
                self.state = FetchDecoderState::AwaitingEntry;
                Ok(Some(header))
            }
            Err(MessageError::UnexpectedEof) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// バッファから FetchStreamEntry のデコードを試みる
    ///
    /// - `Ok(Some(entry))` — デコード成功。group_id / subgroup_id / object_id は絶対値に解決済み。
    ///   Object エントリの場合、呼び出し側は `payload_length` バイト分のペイロードを
    ///   バッファから読み出した後 `consume_payload` を呼ぶこと。
    ///   Object エントリの Object Properties は `properties_bytes` に保持される。
    ///   EndOfRange / EndOfGroup / EndOfTrack エントリの場合、`consume_payload` は不要。
    ///   EndOfGroup / EndOfTrack は marker 自身が ID フィールドを持たないため、
    ///   直前の Object または End of Range の Group ID だけを提供する
    ///   (draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields))。
    /// - `Ok(None)` — データ不足、追加データが必要
    /// - `Err(e)` — デコードエラー
    ///
    /// End of Track の後の Object / End of Range は `MalformedTrack` を返す
    /// (draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 5)。
    /// marker の前後に Object または End of Range が無い marker は `MalformedObject` を返す
    /// (同 §11.4.2.2 の prior Object が存在しない)。
    pub fn try_decode_entry(&mut self) -> Result<Option<DecodedFetchEntry>, MessageError> {
        if !matches!(self.state, FetchDecoderState::AwaitingEntry) {
            return Err(MessageError::ProtocolViolation(
                "not ready to decode entry (header not decoded or payload not consumed)",
            ));
        }
        if self.buf.is_empty() {
            return Ok(None);
        }
        match FetchStreamEntry::decode_with_properties(&self.buf, self.prior_context) {
            Ok((entry, properties_bytes, consumed)) => {
                self.buf.drain(..consumed);
                let decoded = self.resolve_and_transition(entry, properties_bytes)?;
                Ok(Some(decoded))
            }
            Err(MessageError::UnexpectedEof) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// エントリのデルタを解決し、FetchPriorContext を遷移させる
    fn resolve_and_transition(
        &mut self,
        entry: FetchStreamEntry,
        properties_bytes: Option<Vec<u8>>,
    ) -> Result<DecodedFetchEntry, MessageError> {
        // draft-ietf-moq-transport-23 §12.1 (Malformed Tracks) 条件 5:
        // "An Object is received whose Group and Object ID are larger than the final Object in
        // the Track." End of Track の後の Object と End of Range は、Track の最終 Object より
        // 後ろの Location を指すため malformed track になる。marker の重複は受理する
        // (§11.4.2.2 (Prior Object Fields): End of Track は End of Group も意味するため、
        // どちらの marker も直前状態を変えずに読み直せる)。
        if self.track_ended
            && !matches!(
                entry,
                FetchStreamEntry::EndOfGroup | FetchStreamEntry::EndOfTrack
            )
        {
            return Err(MessageError::MalformedTrack(
                "fetch entry received after end of track",
            ));
        }
        match entry {
            FetchStreamEntry::Object(obj) => {
                let resolved = self.resolve_object(&obj, properties_bytes)?;
                self.validate_fetch_object(&resolved)?;

                // prior_state を更新する
                self.prior_state = Some(FetchPriorState {
                    group_id: resolved.group_id,
                    object_id: resolved.object_id,
                    subgroup_id: Some(resolved.subgroup_id),
                    publisher_priority: Some(resolved.publisher_priority),
                });

                // FetchPriorContext を遷移する: Object → HasPriorObject
                self.prior_context = FetchPriorContext::HasPriorObject;

                if resolved.payload_length > 0 {
                    self.state = FetchDecoderState::ConsumingPayload {
                        remaining: resolved.payload_length,
                    };
                }

                Ok(DecodedFetchEntry::Object(resolved))
            }
            FetchStreamEntry::EndOfNonExistentRange {
                group_id_delta,
                object_id_delta,
            } => {
                let (group_id, object_id) =
                    self.resolve_end_of_range(group_id_delta, object_id_delta)?;
                Ok(DecodedFetchEntry::EndOfNonExistentRange {
                    group_id,
                    object_id,
                })
            }
            FetchStreamEntry::EndOfUnknownRange {
                group_id_delta,
                object_id_delta,
            } => {
                let (group_id, object_id) =
                    self.resolve_end_of_range(group_id_delta, object_id_delta)?;
                Ok(DecodedFetchEntry::EndOfUnknownRange {
                    group_id,
                    object_id,
                })
            }
            FetchStreamEntry::EndOfTimedOutRange {
                group_id_delta,
                object_id_delta,
            } => {
                let (group_id, object_id) =
                    self.resolve_end_of_range(group_id_delta, object_id_delta)?;
                Ok(DecodedFetchEntry::EndOfTimedOutRange {
                    group_id,
                    object_id,
                })
            }
            FetchStreamEntry::EndOfGoneRange {
                group_id_delta,
                object_id_delta,
            } => {
                let (group_id, object_id) =
                    self.resolve_end_of_range(group_id_delta, object_id_delta)?;
                Ok(DecodedFetchEntry::EndOfGoneRange {
                    group_id,
                    object_id,
                })
            }
            FetchStreamEntry::EndOfGroup => self.resolve_end_marker(false),
            FetchStreamEntry::EndOfTrack => self.resolve_end_marker(true),
        }
    }

    /// End of Group / End of Track の prior 状態を更新する
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields):
    /// "The prior Object is the last Object in its Group, or in the Track, respectively.
    /// End of Track also indicates End of Group. The prior Object ID becomes 2^64-1."
    ///
    /// marker は ID フィールドを持たないため、Group ID は直前の Object または End of Range の
    /// 解決済み値をそのまま引き継ぎ、Object ID だけを `2^64 - 1` にする。Subgroup ID と
    /// Priority は直前の actual Object の値を保持する (End of Range と同じ扱い)。
    ///
    /// 直前の Object も End of Range も無い場合、`The prior Object is the last Object` が
    /// 指す Object が存在しないため MALFORMED_OBJECT (0x201) にする
    /// (draft-ietf-moq-transport-23 §12.2: "a flag references a prior Object that does not
    /// exist")。
    fn resolve_end_marker(&mut self, track: bool) -> Result<DecodedFetchEntry, MessageError> {
        let Some(prior) = self.prior_state else {
            return Err(MessageError::MalformedObject(
                "end of group or end of track must follow an object or an end of range indicator",
            ));
        };
        self.prior_state = Some(FetchPriorState {
            group_id: prior.group_id,
            object_id: u64::MAX,
            subgroup_id: prior.subgroup_id,
            publisher_priority: prior.publisher_priority,
        });
        if track {
            self.track_ended = true;
            Ok(DecodedFetchEntry::EndOfTrack {
                group_id: prior.group_id,
            })
        } else {
            Ok(DecodedFetchEntry::EndOfGroup {
                group_id: prior.group_id,
            })
        }
    }

    /// End of Range の生値を絶対 Location に解決し、prior 状態を更新する
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.1 (ID Decoding):
    /// - FETCH_HEADER 直後の最初の entry は両フィールドが絶対 ID
    /// - それ以外は直前の Object または End of Range の Location が基準で、
    ///   Group ID は昇順なら加算、降順なら減算する
    /// - Group ID Delta が 0 なら Object ID は prior Object ID + Object ID Delta + 1
    /// - Group ID Delta が 0 でなければ Object ID Delta が新しい Group の絶対 Object ID
    /// - 解決結果が u64 の範囲を外れる場合は DELTA_ENCODING_OVERFLOW (0x400)
    ///
    /// prior Subgroup ID / Priority は End of Range では更新しない
    /// (同 §11.4.2.2 (Prior Object Fields): 直前の actual Object の値を保持する)。
    fn resolve_end_of_range(
        &mut self,
        group_id_delta: u64,
        object_id_delta: u64,
    ) -> Result<(u64, u64), MessageError> {
        let location = match self.prior_state {
            // 最初の entry は絶対 ID
            None => (group_id_delta, object_id_delta),
            Some(prior) => {
                let group_id = match self.group_order {
                    FETCH_GROUP_ORDER_ASCENDING => {
                        prior.group_id.checked_add(group_id_delta).ok_or({
                            MessageError::DeltaEncodingOverflow(
                                "fetch end of range group ID delta overflow in ascending order",
                            )
                        })?
                    }
                    FETCH_GROUP_ORDER_DESCENDING => {
                        prior.group_id.checked_sub(group_id_delta).ok_or({
                            MessageError::DeltaEncodingOverflow(
                                "fetch end of range group ID delta underflow in descending order",
                            )
                        })?
                    }
                    _ => unreachable!(
                        "group_order is validated to ASCENDING or DESCENDING by validate_fetch_group_order"
                    ),
                };
                // Group ID Delta が 0 のときだけ Object ID が prior からの相対値になり、
                // 非 0 のときは Object ID Delta 自体が新しい Group の絶対 Object ID になる。
                let object_id = if group_id_delta == 0 {
                    prior
                        .object_id
                        .checked_add(object_id_delta)
                        .and_then(|v| v.checked_add(1))
                        .ok_or(MessageError::DeltaEncodingOverflow(
                            "fetch end of range object ID delta overflow",
                        ))?
                } else {
                    object_id_delta
                };
                (group_id, object_id)
            }
        };

        self.update_prior_for_end_of_range(location.0, location.1);

        // FetchPriorContext を遷移する:
        // First → NoPriorActualObject, それ以外は変更なし
        if matches!(self.prior_context, FetchPriorContext::First) {
            self.prior_context = FetchPriorContext::NoPriorActualObject;
        }

        Ok(location)
    }

    /// FetchStreamObject のデルタフィールドを絶対値に解決する (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))
    ///
    /// `properties_bytes` は `FetchStreamEntry::decode_with_properties` が返した
    /// Properties Length varint 込みの生バイト列であり、そのまま保持する。
    ///
    /// prior Subgroup ID / Priority は直前の actual Object の値を参照する
    /// (draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields))。そのため
    /// actual Object が未出現のままそれらを参照すると MALFORMED_OBJECT になる。
    /// 同じ規則は低水準 codec も `FetchPriorContext::NoPriorActualObject` で検証しており、
    /// 通常はそちらが先に拒否する。ここでは直前状態を直接見ることで、prior 状態の
    /// 不整合によって不正な参照が 0 などの代替値に化けて素通りすることを防ぐ。
    ///
    /// 同一 Group の Object ID Delta は prior Object ID + delta + 2 として解決する
    /// (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))。End of Range の同一 Group の
    /// +1 (§11.4.2.1 (ID Decoding)) とは別規則であり、共通化しない。
    fn resolve_object(
        &self,
        obj: &FetchStreamObject,
        properties_bytes: Option<Vec<u8>>,
    ) -> Result<DecodedFetchObject, MessageError> {
        let prior = self.prior_state;

        // 最初のオブジェクト: group_id / object_id は絶対値 (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))
        let group_id = if let Some(id) = obj.group_id {
            if prior.is_none() {
                id
            } else {
                let prev_group = prior.map_or(0, |p| p.group_id);
                match self.group_order {
                    FETCH_GROUP_ORDER_ASCENDING => prev_group
                        .checked_add(id)
                        .and_then(|v| v.checked_add(1))
                        .ok_or({
                            MessageError::DeltaEncodingOverflow(
                                "fetch group ID delta overflow in ascending order",
                            )
                        })?,
                    FETCH_GROUP_ORDER_DESCENDING => prev_group
                        .checked_sub(id)
                        .and_then(|v| v.checked_sub(1))
                        .ok_or({
                            MessageError::DeltaEncodingOverflow(
                                "fetch group ID delta underflow in descending order",
                            )
                        })?,
                    _ => unreachable!(
                        "group_order is validated to ASCENDING or DESCENDING by validate_fetch_group_order"
                    ),
                }
            }
        } else {
            prior.map_or(0, |p| p.group_id)
        };

        let subgroup_id = match &obj.subgroup_id {
            FetchSubgroupIdMode::Zero => 0,
            FetchSubgroupIdMode::PreviousSame => prior
                .and_then(|p| p.subgroup_id)
                .ok_or(MessageError::MalformedObject(
                    "fetch object must not reference a prior subgroup ID without a prior actual object",
                ))?,
            FetchSubgroupIdMode::PreviousPlusOne => prior
                .and_then(|p| p.subgroup_id)
                .ok_or(MessageError::MalformedObject(
                    "fetch object must not reference a prior subgroup ID without a prior actual object",
                ))?
                .checked_add(1)
                .ok_or(MessageError::DeltaEncodingOverflow(
                    "fetch subgroup ID delta overflow",
                ))?,
            FetchSubgroupIdMode::Explicit(id) => *id,
        };

        // 最初のオブジェクト: object_id は絶対値
        // Group ID Delta が present のとき: Object ID Delta present → 絶対値、absent → prior + 1
        // Group ID Delta が absent のとき: Object ID Delta present → prior + delta + 2、absent → prior + 1
        // (draft-ietf-moq-transport-23 §11.4.1.1 (Flags))
        //
        // 同一 Group の +2 は End of Range の同一 Group の +1 (§11.4.2.1 (ID Decoding)) とは
        // 別規則である。判定は解決後の Group ID の比較ではなく Group ID Delta フィールドの
        // 有無で行う (仕様の条件そのものであり、解決結果に依存しない)。
        let object_id =
            if prior.is_none() {
                obj.object_id.unwrap_or(0)
            } else {
                let prev_object = prior.map_or(0, |p| p.object_id);
                if obj.group_id.is_some() {
                    // Group 変更時: Object ID Delta は新しい Group の絶対 Object ID、absent なら prior + 1
                    match obj.object_id {
                        Some(absolute_id) => absolute_id,
                        None => prev_object.checked_add(1).ok_or(
                            MessageError::DeltaEncodingOverflow("fetch object ID overflow"),
                        )?,
                    }
                } else if let Some(delta) = obj.object_id {
                    // 同 Group: Object ID = prior + delta + 2
                    prev_object
                        .checked_add(delta)
                        .and_then(|v| v.checked_add(2))
                        .ok_or(MessageError::DeltaEncodingOverflow(
                            "fetch object ID delta overflow",
                        ))?
                } else {
                    // Object ID Delta absent: prior + 1 (group に関係なく)
                    prev_object
                        .checked_add(1)
                        .ok_or(MessageError::DeltaEncodingOverflow(
                            "fetch object ID overflow",
                        ))?
                }
            };

        let publisher_priority = match obj.publisher_priority {
            Some(priority) => priority,
            None => prior.and_then(|p| p.publisher_priority).ok_or(
                MessageError::MalformedObject(
                    "fetch object must not reference a prior publisher priority without a prior actual object",
                ),
            )?,
        };

        Ok(DecodedFetchObject {
            group_id,
            subgroup_id,
            object_id,
            publisher_priority,
            is_datagram_origin: obj.is_datagram_origin,
            payload_length: obj.payload_length,
            properties_bytes,
        })
    }

    /// End of Range 後の prior 状態を更新する
    ///
    /// draft-ietf-moq-transport-23 §11.4.2.2 (Prior Object Fields): prior Group ID /
    /// Object ID は End of Range の解決済み Location に更新するが、prior Subgroup ID /
    /// Priority は End of Range より前の最後の actual Object の値を保持する。
    /// actual Object が未出現のまま End of Range だけが現れた場合は、
    /// Subgroup ID / Priority を `None` のまま残し、後続 Object からの参照を
    /// MALFORMED_OBJECT にする。
    fn update_prior_for_end_of_range(&mut self, group_id: u64, object_id: u64) {
        match &mut self.prior_state {
            Some(prior) => {
                prior.group_id = group_id;
                prior.object_id = object_id;
            }
            None => {
                self.prior_state = Some(FetchPriorState {
                    group_id,
                    object_id,
                    subgroup_id: None,
                    publisher_priority: None,
                });
            }
        }
    }

    /// Fetch オブジェクト列の整合性を検証し、検証用の状態を更新する
    ///
    /// Properties は `obj.properties_bytes` から読み、書式と Property の意味検証だけを
    /// ここで行う。Prior Gap と不存在区間の判定は Track 単位の履歴を持つ `Session` が担う
    /// (単一 stream の Group 前進で他の request と共有する照合情報を消さないため)。
    fn validate_fetch_object(&mut self, obj: &DecodedFetchObject) -> Result<(), MessageError> {
        if let Some(previous) = self.validation.last_object {
            if obj.group_id == previous.group_id && obj.object_id <= previous.object_id {
                return Err(MessageError::ProtocolViolation(
                    "object IDs in the same FETCH response group must be strictly increasing",
                ));
            }

            match self.group_order {
                FETCH_GROUP_ORDER_ASCENDING if obj.group_id < previous.group_id => {
                    return Err(MessageError::ProtocolViolation(
                        "FETCH response groups are not in ascending order",
                    ));
                }
                FETCH_GROUP_ORDER_DESCENDING if obj.group_id > previous.group_id => {
                    return Err(MessageError::ProtocolViolation(
                        "FETCH response groups are not in descending order",
                    ));
                }
                _ => {}
            }

            // group が前進したら過去 group の per-group エントリを prune する
            // draft-ietf-moq-transport-23 §3.2.1 (Fetch Object Delivery):
            // "A publisher MUST send fetched groups in the requested group order (see Section 9.19.8)."
            // group は要求順 (ascending または descending) で送られるため、
            // 過去 group が再出現することはない。
            if obj.group_id != previous.group_id {
                let ascending = self.group_order == FETCH_GROUP_ORDER_ASCENDING;
                self.prune_past_group_state(ascending, obj.group_id);
            }
        }

        if !obj.is_datagram_origin {
            let key = (obj.group_id, obj.subgroup_id);
            if let Some(previous_priority) = self.validation.subgroup_priorities.get(&key)
                && *previous_priority != obj.publisher_priority
            {
                return Err(MessageError::MalformedTrack(
                    "publisher priority changed within the same subgroup in a FETCH response",
                ));
            }
            if let Some(final_object_id) = self.validation.subgroup_final_objects.get(&key)
                && obj.object_id > *final_object_id
            {
                return Err(MessageError::MalformedTrack(
                    "object ID exceeds the known final object for the subgroup",
                ));
            }
            self.validation
                .subgroup_priorities
                .insert(key, obj.publisher_priority);
            self.validation
                .subgroup_max_objects
                .insert(key, obj.object_id);
        }

        // Properties の書式 (長さと KVP 構造) と Property のスコープ・多重可否は
        // 受信 Track の malformed 判定として codec でも検証する
        // (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1
        // (Malformed Tracks) 条件 8 / 9)。Prior Gap と不存在区間の Track 単位の判定は
        // Session が行う。
        if let Some(properties_bytes) = obj.properties_bytes.as_deref() {
            ObjectProperties::decode_exact(properties_bytes)?;
        }
        self.validation.last_object = Some(FetchActualObjectState {
            group_id: obj.group_id,
            object_id: obj.object_id,
        });
        Ok(())
    }

    /// バッファからペイロードの読み出しを試みる
    ///
    /// バッファに `payload_length` バイト分のデータが揃っていれば、
    /// ペイロードを返して状態を AwaitingEntry に遷移する。
    /// データが不足している場合は `None` を返す。
    pub fn try_read_payload(&mut self) -> Option<Vec<u8>> {
        match self.state {
            FetchDecoderState::ConsumingPayload { remaining } => {
                // remaining は wire 由来の payload_length (u64)。32bit 環境で as usize の
                // 切り捨てによる誤読を防ぐため、バッファ長と u64 空間で比較してから変換する。
                // データ不足時は None を返して追加データを待つ意味論のため checked_len は使わない
                if (self.buf.len() as u64) >= remaining {
                    let len = remaining as usize;
                    let payload: Vec<u8> = self.buf.drain(..len).collect();
                    self.state = FetchDecoderState::AwaitingEntry;
                    Some(payload)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    /// ペイロードの消費を通知する
    ///
    /// `try_decode_entry` が返した Object エントリの `payload_length` バイト分を
    /// 呼び出し側が外部で読み出した場合に呼ぶ。
    /// バッファ内のペイロードを読み出す場合は `try_read_payload` を使う。
    ///
    /// `length` は消費したバイト数。
    /// `payload_length` と一致しない、または `ConsumingPayload` 状態以外で
    /// 呼ばれた場合は `Err(ProtocolViolation)` を返し、状態は変わらない。
    pub fn consume_payload(&mut self, length: u64) -> Result<(), MessageError> {
        match self.state {
            FetchDecoderState::ConsumingPayload { remaining } => {
                if length != remaining {
                    return Err(MessageError::ProtocolViolation(
                        "consumed payload length does not match remaining",
                    ));
                }
                self.state = FetchDecoderState::AwaitingEntry;
                Ok(())
            }
            _ => Err(MessageError::ProtocolViolation(
                "consume_payload called in unexpected state",
            )),
        }
    }

    /// これ以上データが来ないことを通知し、stream 境界として完結しているか検証する
    ///
    /// EOF 時点で header 未受信、partial entry header、または未消費ペイロードが
    /// 残っている場合は `UnexpectedEof` を返す。
    ///
    /// ヘッダーのみ + FIN（エントリ 0 個）は正規の空 FETCH 応答として成功を返す。
    /// draft-ietf-moq-transport-23 §3.2.1 (Fetch Object Delivery): "If no Objects exist in the
    /// requested range, the publisher opens the unidirectional stream, sends the
    /// FETCH_HEADER (see Section 11.4.1) and closes the stream with a FIN."
    /// （要求範囲にオブジェクトが 1 つも存在しない場合、publisher は unidirectional ストリームを
    /// 開き FETCH_HEADER を送って FIN で閉じる。空応答が正規なのは §3.2 (Fetch) の
    /// 「track にオブジェクトが 1 つも公開されていない、または Start Location が
    /// Largest Object を上回る場合は MUST REQUEST_ERROR INVALID_RANGE」に該当しない範囲のみ。
    /// この区別は bidi request stream 上の FETCH_OK / REQUEST_ERROR で行われるため、
    /// データストリームの decoder 単体では判別不能）
    /// この節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn finish(&self) -> Result<(), MessageError> {
        match self.state {
            FetchDecoderState::AwaitingHeader => Err(MessageError::UnexpectedEof),
            FetchDecoderState::AwaitingEntry => {
                // 状態が AwaitingEntry である時点でヘッダーはデコード済み。
                // バッファが空ならヘッダーのみの空 FETCH 応答も含めて完結とみなす
                // （partial entry header はバッファ非空として検出される）。
                if self.buf.is_empty() {
                    Ok(())
                } else {
                    Err(MessageError::UnexpectedEof)
                }
            }
            FetchDecoderState::ConsumingPayload { .. } => Err(MessageError::UnexpectedEof),
        }
    }
}

impl Default for FetchStreamDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_fetch_group_order(group_order: u8) -> Result<(), MessageError> {
    match group_order {
        FETCH_GROUP_ORDER_ASCENDING | FETCH_GROUP_ORDER_DESCENDING => Ok(()),
        _ => Err(MessageError::ProtocolViolation(
            "FETCH group order must be 1 (Ascending) or 2 (Descending)",
        )),
    }
}

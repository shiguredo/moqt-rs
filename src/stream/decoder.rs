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
use crate::{error::MessageError, object_properties::ObjectPropertyTracker};
use alloc::vec::Vec;
use hashbrown::HashMap;

/// デコード済み Subgroup オブジェクトの情報
///
/// `SubgroupObject` の生のデルタ値ではなく、絶対値に解決済みの情報を提供する。
/// ペイロードは含まない。呼び出し側が `payload_length` バイト分を消費する。
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedSubgroupObject {
    /// 絶対 Object ID (デルタから解決済み)
    pub object_id: u64,
    /// ペイロード長 (0 の場合は status が存在する)
    pub payload_length: u64,
    /// Object Status (payload_length == 0 の場合のみ)
    pub status: Option<u64>,
    /// Properties の生バイト (Properties Length varint + Properties データ)
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
                // draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header): 初回 Object の Object ID は
                //           Object ID Delta フィールド値そのもの (絶対値)。以降は prev + delta + 1
                let object_id = match self.prev_object_id {
                    None => obj.object_id_delta,
                    Some(prev) => prev
                        .checked_add(obj.object_id_delta)
                        .and_then(|v| v.checked_add(1))
                        .ok_or(MessageError::ProtocolViolation(
                            "subgroup object ID delta overflow",
                        ))?,
                };
                self.prev_object_id = Some(object_id);

                // SubgroupIdMode::FirstObjectId の解決
                if self.is_first_object_id_mode && self.resolved_subgroup_id.is_none() {
                    self.resolved_subgroup_id = Some(object_id);
                }

                // 不変条件: status.is_some() ⇔ payload_length == 0
                // 根拠: draft-ietf-moq-transport-21 §11.3.1 (Subgroup Header)
                // "The Object Status field is only sent if the Object Payload Length is zero"
                // および §11.1.2 (Object Status)
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
    /// draft-ietf-moq-transport-21 §11.3.2 (Closing Subgroup Streams) 冒頭の
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
    /// Subgroup Object due to the subscriber's Forward State" に該当）でも、受信側はワイヤから
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
/// ペイロードは含まない。呼び出し側が `payload_length` バイト分を消費する。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DecodedFetchEntry {
    /// 通常のオブジェクト（デルタ解決済み）
    Object(DecodedFetchObject),
    /// End of Non-Existent Range (0x8C)
    EndOfNonExistentRange {
        /// 終端 Group ID
        group_id: u64,
        /// 終端 Object ID
        object_id: u64,
    },
    /// End of Unknown Range (0x10C)
    EndOfUnknownRange {
        /// 終端 Group ID
        group_id: u64,
        /// 終端 Object ID
        object_id: u64,
    },
    /// End of Timed-Out Range (0x20C, draft-ietf-moq-transport-21 §11.4.1 Table 7)
    EndOfTimedOutRange {
        /// 終端 Group ID
        group_id: u64,
        /// 終端 Object ID
        object_id: u64,
    },
}

/// デコード済み Fetch オブジェクトの情報（デルタ解決済み）
#[derive(Debug, Clone, Copy, PartialEq)]
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

/// 前回のオブジェクト情報（デルタ解決用）
#[derive(Debug, Clone, Copy)]
struct FetchPriorState {
    group_id: u64,
    subgroup_id: u64,
    object_id: u64,
    publisher_priority: u8,
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
    object_properties: ObjectPropertyTracker,
}

const FETCH_GROUP_ORDER_ASCENDING: u8 = 0x01;
const FETCH_GROUP_ORDER_DESCENDING: u8 = 0x02;

/// Fetch レスポンスストリーム用バッファ付きインクリメンタルデコーダー (sans I/O)
///
/// `FetchPriorContext` の状態遷移を内部で自動管理し、
/// デルタ圧縮された group_id / subgroup_id / object_id を絶対値に解決する。
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
///         Some(_) => { /* EndOfRange エントリ */ }
///         None => {
///             decoder.push(&more_data);
///         }
///     }
/// }
/// ```
pub struct FetchStreamDecoder {
    buf: Vec<u8>,
    state: FetchDecoderState,
    /// FETCH response の Group 順序 (draft-ietf-moq-transport-21 §9.20.9 (GROUP ORDER Parameter) / draft-ietf-moq-transport-21 §9.11 (FETCH))
    group_order: u8,
    /// FetchPriorContext の現在の状態
    prior_context: FetchPriorContext,
    /// 前回のオブジェクト情報（デルタ解決用）
    prior_state: Option<FetchPriorState>,
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
            return Err(MessageError::ProtocolViolation(
                "malformed track: object ID exceeds the known final object for the subgroup",
            ));
        }
        self.validation
            .subgroup_final_objects
            .insert(key, final_object_id);
        Ok(())
    }

    /// group 境界での prune: 過去 group の per-group 検証状態をクリアする
    ///
    /// draft-ietf-moq-transport-21 §9.11 (FETCH):
    /// "A publisher MUST send fetched groups in the requested group order, either ascending
    /// or descending."
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
        self.validation
            .object_properties
            .prune_past_groups(ascending, current_group);
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
    ///   EndOfRange エントリの場合、`consume_payload` は不要。
    /// - `Ok(None)` — データ不足、追加データが必要
    /// - `Err(e)` — デコードエラー
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
        match entry {
            FetchStreamEntry::Object(obj) => {
                let resolved = self.resolve_object(&obj)?;
                self.validate_fetch_object(&resolved, properties_bytes.as_deref())?;

                // prior_state を更新する
                self.prior_state = Some(FetchPriorState {
                    group_id: resolved.group_id,
                    subgroup_id: resolved.subgroup_id,
                    object_id: resolved.object_id,
                    publisher_priority: resolved.publisher_priority,
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
                group_id,
                object_id,
            } => {
                // End of Range エントリ: group_id / object_id を prior_state に反映する
                // (draft-ietf-moq-transport-21 §11.4.1.2 (End of Range): prior Group ID / Object ID は End of Range の値を使う)
                self.update_prior_for_end_of_range(group_id, object_id);

                // FetchPriorContext を遷移する:
                // First → NoPriorActualObject, それ以外は変更なし
                if matches!(self.prior_context, FetchPriorContext::First) {
                    self.prior_context = FetchPriorContext::NoPriorActualObject;
                }

                Ok(DecodedFetchEntry::EndOfNonExistentRange {
                    group_id,
                    object_id,
                })
            }
            FetchStreamEntry::EndOfUnknownRange {
                group_id,
                object_id,
            } => {
                self.update_prior_for_end_of_range(group_id, object_id);

                if matches!(self.prior_context, FetchPriorContext::First) {
                    self.prior_context = FetchPriorContext::NoPriorActualObject;
                }

                Ok(DecodedFetchEntry::EndOfUnknownRange {
                    group_id,
                    object_id,
                })
            }
            FetchStreamEntry::EndOfTimedOutRange {
                group_id,
                object_id,
            } => {
                self.update_prior_for_end_of_range(group_id, object_id);

                if matches!(self.prior_context, FetchPriorContext::First) {
                    self.prior_context = FetchPriorContext::NoPriorActualObject;
                }

                Ok(DecodedFetchEntry::EndOfTimedOutRange {
                    group_id,
                    object_id,
                })
            }
        }
    }

    /// FetchStreamObject のデルタフィールドを絶対値に解決する (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
    fn resolve_object(&self, obj: &FetchStreamObject) -> Result<DecodedFetchObject, MessageError> {
        let prior = self.prior_state;

        // 最初のオブジェクト: group_id / object_id は絶対値 (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
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
                            MessageError::ProtocolViolation(
                                "fetch group ID delta overflow in ascending order",
                            )
                        })?,
                    FETCH_GROUP_ORDER_DESCENDING => prev_group
                        .checked_sub(id)
                        .and_then(|v| v.checked_sub(1))
                        .ok_or({
                            MessageError::ProtocolViolation(
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
            FetchSubgroupIdMode::PreviousSame => prior.map_or(0, |p| p.subgroup_id),
            FetchSubgroupIdMode::PreviousPlusOne => prior.map_or(Ok(1), |p| {
                p.subgroup_id
                    .checked_add(1)
                    .ok_or(MessageError::ProtocolViolation(
                        "fetch subgroup ID delta overflow",
                    ))
            })?,
            FetchSubgroupIdMode::Explicit(id) => *id,
        };

        // 最初のオブジェクト: object_id は絶対値
        // Group 変更時: Object ID Delta present → 絶対値、absent → prior + 1
        // 同 Group: Object ID Delta present → prior + delta、absent → prior + 1
        // (draft-ietf-moq-transport-21 §11.4.1.1 (Flags))
        // 注: Subgroup (§11.3.1) と異なり Fetch の Object ID Delta に +1 は付かない
        let object_id = if prior.is_none() {
            obj.object_id.unwrap_or(0)
        } else {
            let prev_group = prior.map_or(0, |p| p.group_id);
            let prev_object = prior.map_or(0, |p| p.object_id);
            if group_id != prev_group {
                // Group 変更時: Object ID Delta は絶対値、absent なら prior + 1
                match obj.object_id {
                    Some(absolute_id) => absolute_id,
                    None => prev_object
                        .checked_add(1)
                        .ok_or(MessageError::ProtocolViolation("fetch object ID overflow"))?,
                }
            } else if let Some(delta) = obj.object_id {
                // 同 Group: Object ID = prior + delta (Subgroup と異なり +1 しない)
                prev_object
                    .checked_add(delta)
                    .ok_or(MessageError::ProtocolViolation(
                        "fetch object ID delta overflow",
                    ))?
            } else {
                // Object ID Delta absent: prior + 1 (group に関係なく)
                prev_object
                    .checked_add(1)
                    .ok_or(MessageError::ProtocolViolation("fetch object ID overflow"))?
            }
        };

        let publisher_priority = obj
            .publisher_priority
            .unwrap_or_else(|| prior.map_or(0, |p| p.publisher_priority));

        Ok(DecodedFetchObject {
            group_id,
            subgroup_id,
            object_id,
            publisher_priority,
            is_datagram_origin: obj.is_datagram_origin,
            payload_length: obj.payload_length,
        })
    }

    /// End of Range エントリ後に prior_state の group_id / object_id を更新する
    fn update_prior_for_end_of_range(&mut self, group_id: u64, object_id: u64) {
        match &mut self.prior_state {
            Some(prior) => {
                prior.group_id = group_id;
                prior.object_id = object_id;
            }
            None => {
                // prior_state が無い場合は部分的な状態を作る
                // subgroup_id と publisher_priority は不明だが、
                // NoPriorActualObject 状態ではこれらの prior 参照は禁止されるため安全
                self.prior_state = Some(FetchPriorState {
                    group_id,
                    subgroup_id: 0,
                    object_id,
                    publisher_priority: 0,
                });
            }
        }
    }

    fn validate_fetch_object(
        &mut self,
        obj: &DecodedFetchObject,
        properties_bytes: Option<&[u8]>,
    ) -> Result<(), MessageError> {
        if let Some(previous) = self.validation.last_object {
            if obj.group_id == previous.group_id && obj.object_id <= previous.object_id {
                return Err(MessageError::ProtocolViolation(
                    "malformed track: object IDs in the same FETCH response group must be strictly increasing",
                ));
            }

            match self.group_order {
                FETCH_GROUP_ORDER_ASCENDING if obj.group_id < previous.group_id => {
                    return Err(MessageError::ProtocolViolation(
                        "malformed track: FETCH response groups are not in ascending order",
                    ));
                }
                FETCH_GROUP_ORDER_DESCENDING if obj.group_id > previous.group_id => {
                    return Err(MessageError::ProtocolViolation(
                        "malformed track: FETCH response groups are not in descending order",
                    ));
                }
                _ => {}
            }

            // group が前進したら過去 group の per-group エントリを prune する
            // draft-ietf-moq-transport-21 §9.11 (FETCH):
            // "A publisher MUST send fetched groups in the requested group order, either ascending
            // or descending."
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
                return Err(MessageError::ProtocolViolation(
                    "malformed track: publisher priority changed within the same subgroup in a FETCH response",
                ));
            }
            if let Some(final_object_id) = self.validation.subgroup_final_objects.get(&key)
                && obj.object_id > *final_object_id
            {
                return Err(MessageError::ProtocolViolation(
                    "malformed track: object ID exceeds the known final object for the subgroup",
                ));
            }
            self.validation
                .subgroup_priorities
                .insert(key, obj.publisher_priority);
            self.validation
                .subgroup_max_objects
                .insert(key, obj.object_id);
        }

        self.validation.object_properties.observe_object(
            obj.group_id,
            obj.object_id,
            properties_bytes,
        )?;
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
    /// draft-ietf-moq-transport-21 §9.11 (FETCH): "If no Objects exist in the
    /// requested range, the publisher opens the unidirectional stream, sends the
    /// FETCH_HEADER (see Section 11.4.1) and closes the stream with a FIN."
    /// （要求範囲にオブジェクトが 1 つも存在しない場合、publisher は unidirectional ストリームを
    /// 開き FETCH_HEADER を送って FIN で閉じる。空応答が正規なのは §9.11 の
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

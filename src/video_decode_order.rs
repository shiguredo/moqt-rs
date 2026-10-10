//! 受信した映像 Object を復号してよいかを Group の順序と欠落から決める状態機械
//!
//! draft-ietf-moq-transport-22 §2.1.2 (Object States): "Since Objects can be delivered out
//! of order" であり、Group ごとに別の Subgroup ストリームで届くため、前の Group の末尾が
//! 次の Group の先頭より後に届くことがある。次の Group のキーフレームを復号した後に前の
//! Group の delta を復号すると、参照フレームが壊れて映像が崩れる。Group 内で Object が
//! 欠けた後の delta も、参照するフレームが無いまま復号することになる。
//!
//! [`VideoDecodeOrder`] は復号してよい Object だけを通す。
//!
//! - 復号中の Group より古い Group の Object は捨てる
//! - 同じ Group で直前に復号した Object ID 以前の Object は捨てる (重複か遅着)
//! - キーフレームは参照を持たないため、古い Group でない限り復号を始め直す (復号中の Group より
//!   新しい Group のキーフレームで新しい Group へ移り、Group の先頭が遅れて届いた場合は同じ
//!   Group でも前より前の Object ID から始め直す)
//! - 同じ Group の delta は、直前に復号した Object の次の Object ID のときだけ通す
//! - 間の Object ID が欠けている場合は、Prior Object ID Gap (§10.9 (Prior Object ID Gap))
//!   がその分の Object の非存在を示すときに限り連続とみなす。§2.1.2 は "A gap in the
//!   observed Object IDs does not by itself convey any information about the skipped
//!   Objects" とするため、示されない欠けは欠落として扱い、次のキーフレームまで delta を
//!   捨てる
//! - 欠落の後でキーフレームを待っている間も、欠落を検出した Group を保持する。それより
//!   古い Group の Object を古いとして捨てるためである
//!
//! 1 Group の Object を 1 本の Subgroup で送る publisher を前提とする (draft-ietf-moq-transport-22
//! §2.2 (Subgroups): "A subgroup is a sequence of one or more objects from the same group in
//! ascending order by Object ID.")。1 Group を複数の Subgroup に分ける publisher の Object ID の
//! 飛びも欠落として扱う。どの Object を参照しているかを受信側は判断できないため、崩れた映像
//! ではなくキーフレーム待ちに倒す。
//!
//! 判定に使うのは Object の位置と種別だけで、時刻も I/O も扱わない (sans I/O)。購読
//! (decoder) ごとに [`VideoDecodeOrder`] を 1 つ持ち、decoder を作り直したら
//! [`VideoDecodeOrder::reset`] で初期状態に戻す。
//!
//! 根拠の仕様はドラフトであり、将来変更される可能性がある。

use crate::error::MessageError;
use crate::object_properties::ObjectProperties;

/// 復号しない理由
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoObjectSkipReason {
    /// 復号中の Group より古い Group の Object、または直前に復号した Object 以前の
    /// Object ID (重複か遅着)
    Stale,
    /// 参照するフレームが欠けているため、キーフレームを待っている
    MissingReference,
}

/// Object を復号してよいかの判定結果
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoObjectAdmission {
    /// 復号してよい
    Decode,
    /// 復号しない
    Skip {
        /// 復号しない理由
        reason: VideoObjectSkipReason,
    },
}

/// 判定に使う Object の位置と種別
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoObjectPosition {
    /// Object の Group ID
    pub group_id: u64,
    /// Group 内の Object ID
    pub object_id: u64,
    /// キーフレーム (他のフレームを参照しない) かどうか
    pub is_key_frame: bool,
    /// この Object の直前にあり、存在しない (今後も存在しない) Object の数
    ///
    /// PRIOR_OBJECT_ID_GAP (draft-ietf-moq-transport-22 §10.9 (Prior Object ID Gap)) の値。
    /// Property が無い Object は 0 とする ([`prior_object_id_gap_of`] で読む)。
    pub prior_object_id_gap: u64,
}

/// 映像 Object の復号順を判定する状態機械
///
/// 購読 (decoder) ごとに 1 つ持つ。decoder を作り直したら [`VideoDecodeOrder::reset`] で
/// 初期状態に戻し、次のキーフレームから復号を始める。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoDecodeOrder {
    /// 復号中の Group。復号を始めていなければ `None`
    ///
    /// 欠落の後でキーフレームを待っている間も、欠落を検出した Group を保持する。
    /// それより古い Group の Object を古いとして捨てるためである。
    decoding_group_id: Option<u64>,
    /// 復号中の Group で最後に復号した Object ID。キーフレームを待っている間は `None`
    last_decoded_object_id: Option<u64>,
    /// 復号の開始前と、欠落を検出した後は `true`。キーフレームを通すまで delta を捨てる
    awaiting_key_frame: bool,
}

impl VideoDecodeOrder {
    /// 復号を 1 つも始めていない状態で作る
    ///
    /// 最初に通すのはキーフレームだけである。購読が Group の途中から始まった場合、最初に
    /// 届く delta は参照するフレームを復号していない。
    pub fn new() -> Self {
        Self {
            decoding_group_id: None,
            last_decoded_object_id: None,
            awaiting_key_frame: true,
        }
    }

    /// Object を復号してよいかを判定し、復号する場合は状態を進める
    ///
    /// 判定は Object の位置 (`group_id` / `object_id`)、種別 (`is_key_frame`)、
    /// Prior Object ID Gap (`prior_object_id_gap`) だけで決まる。
    ///
    /// - [`VideoObjectAdmission::Decode`] — 復号してよい。状態は「この Object を復号した」へ進む
    /// - [`VideoObjectAdmission::Skip`] — 復号しない。理由は [`VideoObjectSkipReason`]
    ///   - `Stale` は状態を変えない
    ///   - `MissingReference` はキーフレームを待つ Group を保持するために状態を進める
    ///     (古い Group の Object を古いとして捨て続けるため)
    pub fn admit(&mut self, position: &VideoObjectPosition) -> VideoObjectAdmission {
        let VideoObjectPosition {
            group_id,
            object_id,
            is_key_frame,
            prior_object_id_gap,
        } = *position;

        // 復号中の Group より古い Group の Object は、どれも復号しない
        if let Some(decoding_group_id) = self.decoding_group_id
            && group_id < decoding_group_id
        {
            return VideoObjectAdmission::Skip {
                reason: VideoObjectSkipReason::Stale,
            };
        }

        // 同じ Group で直前に復号した Object 以前の Object ID は、重複か遅着である
        if self.decoding_group_id == Some(group_id)
            && let Some(last_decoded_object_id) = self.last_decoded_object_id
            && object_id <= last_decoded_object_id
        {
            return VideoObjectAdmission::Skip {
                reason: VideoObjectSkipReason::Stale,
            };
        }

        if is_key_frame {
            // キーフレームは参照を持たないため、ここまで来れば復号を始められる
            self.start_decoding(group_id, object_id);
            return VideoObjectAdmission::Decode;
        }

        // 新しい Group のキーフレームを受けないまま、その Group の delta が届いた。
        // 前の Group の残りは古くなるため、この Group でキーフレームを待つ
        if self
            .decoding_group_id
            .is_none_or(|decoding_group_id| group_id > decoding_group_id)
        {
            self.await_missing_reference(group_id);
            return VideoObjectAdmission::Skip {
                reason: VideoObjectSkipReason::MissingReference,
            };
        }

        if self.awaiting_key_frame {
            return VideoObjectAdmission::Skip {
                reason: VideoObjectSkipReason::MissingReference,
            };
        }

        // 復号を始めていれば直前に復号した Object ID がある。無い場合はキーフレーム待ちで
        // あり、上の検査で弾かれている
        let Some(last_decoded_object_id) = self.last_decoded_object_id else {
            return VideoObjectAdmission::Skip {
                reason: VideoObjectSkipReason::MissingReference,
            };
        };

        // 直前に復号した Object との間の Object ID の欠けが、Prior Object ID Gap で
        // 非存在と示された範囲に収まるときだけ連続とみなす。上の検査で
        // `object_id > last_decoded_object_id` は保証されるが、減算は飽和に寄せて
        // 任意の入力列でも panic しないようにする
        let skipped = object_id
            .saturating_sub(last_decoded_object_id)
            .saturating_sub(1);
        if skipped > prior_object_id_gap {
            self.await_missing_reference(group_id);
            return VideoObjectAdmission::Skip {
                reason: VideoObjectSkipReason::MissingReference,
            };
        }

        self.last_decoded_object_id = Some(object_id);
        VideoObjectAdmission::Decode
    }

    /// 初期状態に戻す。次のキーフレームから復号を始める
    ///
    /// decoder を作り直したとき (映像のトラックを切り替えたときなど) に呼ぶ。前の decoder で
    /// 最後に復号した Object を持ち越すと、新しい decoder が参照を持たない Object を通して
    /// しまう。
    pub fn reset(&mut self) {
        self.decoding_group_id = None;
        self.last_decoded_object_id = None;
        self.awaiting_key_frame = true;
    }

    /// 復号中の Group。復号を始めていなければ `None`
    ///
    /// 欠落の後でキーフレームを待っている間も、欠落を検出した Group を返す。
    pub fn decoding_group_id(&self) -> Option<u64> {
        self.decoding_group_id
    }

    /// 復号中の Group で最後に復号した Object ID
    ///
    /// 復号を始めていないときと、キーフレームを待っているときは `None`。
    pub fn last_decoded_object_id(&self) -> Option<u64> {
        self.last_decoded_object_id
    }

    /// キーフレームを待っているかどうか
    ///
    /// 復号の開始前と、欠落を検出した後は `true`。`true` の間はキーフレーム以外を通さない。
    pub fn awaiting_key_frame(&self) -> bool {
        self.awaiting_key_frame
    }

    /// この Object から復号を始める
    fn start_decoding(&mut self, group_id: u64, object_id: u64) {
        self.decoding_group_id = Some(group_id);
        self.last_decoded_object_id = Some(object_id);
        self.awaiting_key_frame = false;
    }

    /// 欠落を検出した Group で、キーフレームを待つ
    fn await_missing_reference(&mut self, group_id: u64) {
        self.decoding_group_id = Some(group_id);
        self.last_decoded_object_id = None;
        self.awaiting_key_frame = true;
    }
}

impl Default for VideoDecodeOrder {
    fn default() -> Self {
        Self::new()
    }
}

/// Object Properties から Prior Object ID Gap の値を取り出す。Property が無ければ 0
///
/// `properties_bytes` は Properties Length varint を含む Object Properties の生バイト列
/// (Subgroup Object / Object Datagram / FETCH 応答の Object はいずれもこの表現で届く)。
/// LOC の Public Properties は同じブロックに載るため (draft-ietf-moq-loc-04 §2.2 (MOQ Object
/// Mapping))、LOC の Property を含むバイト列をそのまま渡してよい。
///
/// draft-ietf-moq-transport-22 §10.9 (Prior Object ID Gap): Prior Object ID Gap は、この
/// Object の直前にあり、存在しない (今後も存在しない) Object の数を示す。Property が
/// 無ければ受信側は直前の Object の存在について何も推論できない (§10.9 / §2.1.2) ため、
/// 0 (非存在と示された Object が無い) として扱う。
///
/// Properties の malformed 判定 (同じ Property の重複など) は受信時に済んでいる前提である。
/// それでも、書式違反を「Property 無し (0)」に潰さないよう、受信経路と同じ framing 検証
/// (宣言長と実データ長の一致) を通す。
///
/// # Errors
///
/// Object Properties の framing が不正 (Length varint が途中で切れている、宣言長が実データ
/// 長と一致しない、Key-Value-Pair が malformed) の場合は
/// [`MessageError::ProtocolViolation`]。
pub fn prior_object_id_gap_of(properties_bytes: Option<&[u8]>) -> Result<u64, MessageError> {
    let Some(properties_bytes) = properties_bytes else {
        return Ok(0);
    };
    let properties = ObjectProperties::decode_exact(properties_bytes)?;
    Ok(properties.prior_object_id_gap().unwrap_or(0))
}

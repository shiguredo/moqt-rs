//! 映像 Object の復号順を判定する規則のテスト
//!
//! [`VideoDecodeOrder`] の公開 API の契約を確認する。Group ごとに別の Subgroup ストリームで
//! 届くため、前の Group の末尾が次の Group の先頭より後に届く (draft-ietf-moq-transport-22
//! §2.1.2 (Object States): "Since Objects can be delivered out of order")。前の Group の
//! delta を復号すると参照フレームが壊れるため、復号してよい Object だけを通す。
//!
//! ここでは実際に起きる到着の形と、捨てる理由 ([`VideoObjectSkipReason::Stale`] と
//! [`VideoObjectSkipReason::MissingReference`]) の区別を固定する。任意の Object 列に対する
//! 性質は `pbt/tests/prop_video_decode_order.rs` が固定する。

use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::object_properties::{
    ObjectProperties, ObjectProperty, ObjectPropertyValue, PROP_PRIOR_OBJECT_ID_GAP,
};
use shiguredo_moqt::track_properties::PROP_IMMUTABLE_PROPERTIES;
use shiguredo_moqt::varint;
use shiguredo_moqt::video_decode_order::{
    VideoDecodeOrder, VideoObjectAdmission, VideoObjectPosition, VideoObjectSkipReason,
    prior_object_id_gap_of,
};

/// 復号してよい
const DECODE: VideoObjectAdmission = VideoObjectAdmission::Decode;

/// 古いか重複のため復号しない
const STALE: VideoObjectAdmission = VideoObjectAdmission::Skip {
    reason: VideoObjectSkipReason::Stale,
};

/// 参照するフレームが欠けているため復号しない
const MISSING_REFERENCE: VideoObjectAdmission = VideoObjectAdmission::Skip {
    reason: VideoObjectSkipReason::MissingReference,
};

/// キーフレーム (他のフレームを参照しない) の位置を作る
fn key(group_id: u64, object_id: u64) -> VideoObjectPosition {
    VideoObjectPosition {
        group_id,
        object_id,
        is_key_frame: true,
        prior_object_id_gap: 0,
    }
}

/// delta (直前のフレームを参照する) の位置を作る
fn delta(group_id: u64, object_id: u64, prior_object_id_gap: u64) -> VideoObjectPosition {
    VideoObjectPosition {
        group_id,
        object_id,
        is_key_frame: false,
        prior_object_id_gap,
    }
}

/// 次の Group のキーフレームの後に届いた前の Group の delta は古いとして捨てる
///
/// Group ごとに別の Subgroup ストリームで届くため、前の Group の末尾が次の Group の先頭より
/// 後に届く (draft-ietf-moq-transport-22 §2.1.2)。前の Group の delta を復号すると、次の
/// Group のキーフレームから始めた参照が壊れる。
#[test]
fn delta_of_older_group_after_newer_group_keyframe_is_stale() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&key(0, 0)),
        DECODE,
        "Group 0 のキーフレームから復号を始められること"
    );
    assert_eq!(
        order.admit(&delta(0, 1, 0)),
        DECODE,
        "Group 0 で連続する delta は復号できること"
    );
    assert_eq!(
        order.admit(&key(1, 0)),
        DECODE,
        "新しい Group 1 のキーフレームで復号を始め直せること"
    );
    assert_eq!(
        order.admit(&delta(0, 2, 0)),
        STALE,
        "Group 1 を復号した後に届いた Group 0 の delta は古いと判定されること"
    );
    assert_eq!(
        order.admit(&delta(1, 1, 0)),
        DECODE,
        "Group 1 の連続する delta は復号できること"
    );
    assert_eq!(
        order.admit(&delta(0, 3, 0)),
        STALE,
        "遅れて届いた Group 0 の delta は古いと判定されること"
    );
    assert_eq!(
        order.admit(&delta(1, 2, 0)),
        DECODE,
        "Group 1 の復号は続けられること"
    );
}

/// Group 内で Object が欠けたら、次のキーフレームまで delta を捨てる
///
/// Object 2 が届かない (損失、または reset された Subgroup の残り)。Object 3 以降は参照先が
/// 無いため復号しない。draft-ietf-moq-transport-22 §2.1.2 は "A gap in the observed Object IDs
/// does not by itself convey any information about the skipped Objects" とするため、欠けた
/// Object が存在しないとは言えない。
#[test]
fn delta_after_a_gap_is_missing_reference_until_the_next_keyframe() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&key(0, 0)),
        DECODE,
        "キーフレームから復号を始めること"
    );
    assert_eq!(
        order.admit(&delta(0, 1, 0)),
        DECODE,
        "直前の次の Object ID の delta は復号できること"
    );
    assert_eq!(
        order.admit(&delta(0, 3, 0)),
        MISSING_REFERENCE,
        "Object 2 の欠けを検出したら復号しないこと"
    );
    assert_eq!(
        order.admit(&delta(0, 4, 0)),
        MISSING_REFERENCE,
        "欠落の後はキーフレームまで delta を捨てること"
    );
    assert_eq!(
        order.admit(&delta(0, 2, 0)),
        MISSING_REFERENCE,
        "欠けた Object が後から届いても、その先を復号できるようにはならないこと"
    );
    assert_eq!(
        order.admit(&key(1, 0)),
        DECODE,
        "次の Group のキーフレームで復号を始め直せること"
    );
    assert_eq!(
        order.admit(&delta(1, 1, 0)),
        DECODE,
        "始め直した Group の delta は復号できること"
    );
}

/// Prior Object ID Gap が示す非存在の範囲に収まる欠けは連続とみなす
///
/// draft-ietf-moq-transport-22 §10.9 (Prior Object ID Gap): Object 3 の Prior Object ID Gap が
/// 2 なら Object 1 と 2 は存在しない。Object 3 は Object 0 の次に存在する Object である。
#[test]
fn prior_object_id_gap_covers_the_missing_objects() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&key(0, 0)),
        DECODE,
        "キーフレームから復号を始めること"
    );
    assert_eq!(
        order.admit(&delta(0, 3, 2)),
        DECODE,
        "非存在と示された Object 1 と 2 を飛ばして復号できること"
    );
    assert_eq!(
        order.admit(&delta(0, 4, 0)),
        DECODE,
        "飛ばした後は連続する delta として復号できること"
    );
}

/// Prior Object ID Gap が欠けの一部しか覆わなければ欠落として扱う
///
/// Object 4 の Prior Object ID Gap は 2 (Object 2 と 3 が存在しない) だが、Object 1 は非存在と
/// 示されていない。draft-ietf-moq-transport-22 §2.1.2 は欠けた Object ID がそれだけでは何も
/// 示さないとするため、Object 1 は届いていないだけかもしれない。
#[test]
fn prior_object_id_gap_covering_only_part_of_the_gap_is_missing_reference() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&key(0, 0)),
        DECODE,
        "キーフレームから復号を始めること"
    );
    assert_eq!(
        order.admit(&delta(0, 4, 2)),
        MISSING_REFERENCE,
        "非存在と示されていない欠けが残るため復号しないこと"
    );
    assert_eq!(
        order.last_decoded_object_id(),
        None,
        "欠落を検出したら復号中の Object ID を捨てること"
    );
    assert_eq!(
        order.decoding_group_id(),
        Some(0),
        "欠落を検出した Group は保持すること"
    );
}

/// キーフレームより前に届いた delta は捨てる
///
/// 購読を Group の途中から始めた場合と、新しい Group の delta がキーフレームより先に届いた
/// 場合。どちらも参照先のキーフレームを復号していない。
#[test]
fn delta_before_any_keyframe_is_missing_reference() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&delta(3, 5, 0)),
        MISSING_REFERENCE,
        "購読を始めた直後の delta は復号しないこと"
    );
    assert_eq!(
        order.admit(&key(4, 0)),
        DECODE,
        "キーフレームが届いたら復号を始められること"
    );
    assert_eq!(
        order.admit(&delta(5, 1, 0)),
        MISSING_REFERENCE,
        "新しい Group の delta はキーフレームが無いため復号しないこと"
    );
    assert_eq!(
        order.admit(&delta(4, 1, 0)),
        STALE,
        "Group 5 でキーフレームを待ち始めたので Group 4 の残りは古いと判定されること"
    );
    assert_eq!(
        order.admit(&key(5, 0)),
        DECODE,
        "待っていた Group 5 のキーフレームで復号を始められること"
    );
}

/// 直前に復号した Object 以前の Object ID は重複か遅着として捨てる
#[test]
fn object_id_not_greater_than_last_decoded_is_stale() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&key(0, 0)),
        DECODE,
        "キーフレームから復号を始めること"
    );
    assert_eq!(
        order.admit(&delta(0, 1, 0)),
        DECODE,
        "直前の次の Object ID の delta は復号できること"
    );
    assert_eq!(
        order.admit(&delta(0, 1, 0)),
        STALE,
        "同じ Object ID の再受信は重複として捨てること"
    );
    assert_eq!(
        order.admit(&key(0, 0)),
        STALE,
        "復号済みの Object ID のキーフレームも重複として捨てること"
    );
    assert_eq!(
        order.admit(&delta(0, 2, 0)),
        DECODE,
        "重複を捨てても後続の delta は復号できること"
    );
}

/// 同じ Group のキーフレームでも、欠落の後なら復号を始め直す
///
/// キーフレームは参照を持たないため、Group が変わらなくても復号を始め直せる。復号中の Group
/// より古い Group のキーフレームは、参照が壊れるため捨てる。
#[test]
fn keyframe_restarts_decoding_even_in_the_same_group() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&key(0, 0)),
        DECODE,
        "キーフレームから復号を始めること"
    );
    assert_eq!(
        order.admit(&delta(0, 3, 0)),
        MISSING_REFERENCE,
        "Object 1 と 2 の欠けで復号を止めること"
    );
    assert_eq!(
        order.admit(&key(0, 5)),
        DECODE,
        "同じ Group のキーフレームで復号を始め直せること"
    );
    assert_eq!(
        order.admit(&delta(0, 6, 0)),
        DECODE,
        "始め直した後の delta は復号できること"
    );
    assert_eq!(
        order.admit(&key(1, 0)),
        DECODE,
        "新しい Group でも始め直せること"
    );
    assert_eq!(
        order.admit(&key(0, 7)),
        STALE,
        "復号中の Group より古い Group のキーフレームは捨てること"
    );
}

/// 欠落の後に届いた同じ Group のキーフレームは、前より前の Object ID でも復号を始め直す
///
/// キーフレームは参照を持たないため、Group の先頭が遅れて届いた場合は直前に復号した Object
/// より前の Object ID から始め直せる (表示順はタイムスタンプで決まる)。復号を続けている間の
/// キーフレームの再受信は、重複として捨てる。
#[test]
fn keyframe_after_a_gap_restarts_from_an_earlier_object_id() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&key(0, 0)),
        DECODE,
        "キーフレームから復号を始めること"
    );
    assert_eq!(
        order.admit(&delta(0, 1, 0)),
        DECODE,
        "連続する delta は復号できること"
    );
    assert_eq!(
        order.admit(&delta(0, 3, 0)),
        MISSING_REFERENCE,
        "Object 2 の欠けで復号を止めること"
    );
    assert_eq!(
        order.admit(&key(0, 0)),
        DECODE,
        "遅れて届いた Group の先頭は、前より前の Object ID でも復号を始め直せること"
    );
    assert_eq!(
        order.admit(&delta(0, 1, 0)),
        DECODE,
        "始め直した後の delta は復号できること"
    );
    assert_eq!(
        order.admit(&key(0, 0)),
        STALE,
        "復号を続けている間に届いたキーフレームの再受信は重複として捨てること"
    );
}

/// reset の後はキーフレームから復号を始め、以前の Group より古い Group も古いとしない
///
/// decoder を作り直したときは復号の状態を持ち越さない。
#[test]
fn reset_starts_decoding_from_a_keyframe() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.admit(&key(7, 0)),
        DECODE,
        "キーフレームから復号を始めること"
    );
    assert_eq!(
        order.admit(&delta(7, 1, 0)),
        DECODE,
        "連続する delta は復号できること"
    );

    order.reset();
    assert_eq!(
        order.admit(&delta(7, 2, 0)),
        MISSING_REFERENCE,
        "reset の前に復号した Object の次でも、キーフレームを復号し直すまでは参照先が無いこと"
    );

    order.reset();
    assert_eq!(
        order.admit(&key(2, 0)),
        DECODE,
        "reset の前に復号していた Group 7 より古い Group 2 から始められること"
    );
    assert_eq!(
        order.admit(&delta(2, 1, 0)),
        DECODE,
        "reset 後も復号の状態は進むこと"
    );
}

/// アクセサが判定のたびの状態を返す
///
/// 復号中の Group はキーフレームを待っている間も保持し、直前に復号した Object ID と
/// キーフレーム待ちのフラグはそこで戻る。`Stale` は状態を変えない。
#[test]
fn accessors_report_the_state_after_each_admission() {
    let mut order = VideoDecodeOrder::new();
    assert_eq!(
        order.decoding_group_id(),
        None,
        "復号を始める前は復号中の Group が無いこと"
    );
    assert_eq!(
        order.last_decoded_object_id(),
        None,
        "復号を始める前は復号した Object ID が無いこと"
    );
    assert!(
        order.awaiting_key_frame(),
        "復号を始める前はキーフレームを待つこと"
    );

    assert_eq!(
        order.admit(&key(2, 0)),
        DECODE,
        "キーフレームから復号を始めること"
    );
    assert_eq!(
        order.decoding_group_id(),
        Some(2),
        "復号中の Group を保持すること"
    );
    assert_eq!(
        order.last_decoded_object_id(),
        Some(0),
        "復号した Object ID を保持すること"
    );
    assert!(
        !order.awaiting_key_frame(),
        "復号を始めたらキーフレーム待ちを解除すること"
    );

    assert_eq!(
        order.admit(&delta(2, 3, 0)),
        MISSING_REFERENCE,
        "Object 1 と 2 の欠けで復号を止めること"
    );
    assert_eq!(
        order.decoding_group_id(),
        Some(2),
        "欠落の後も欠落を検出した Group を保持すること"
    );
    assert_eq!(
        order.last_decoded_object_id(),
        None,
        "欠落の後は復号した Object ID を捨てること"
    );
    assert!(
        order.awaiting_key_frame(),
        "欠落の後はキーフレームを待つこと"
    );

    let before = (
        order.decoding_group_id(),
        order.last_decoded_object_id(),
        order.awaiting_key_frame(),
    );
    assert_eq!(
        order.admit(&delta(1, 0, 0)),
        STALE,
        "復号中の Group より古い Group の Object は古いと判定されること"
    );
    assert_eq!(
        (
            order.decoding_group_id(),
            order.last_decoded_object_id(),
            order.awaiting_key_frame(),
        ),
        before,
        "Stale の判定は状態を変えないこと"
    );
}

/// Object Properties から Prior Object ID Gap を読む
///
/// Property が無ければ 0 とし (draft-ietf-moq-transport-22 §10.9)、書式違反は「Property 無し」
/// に潰さない。
#[test]
fn prior_object_id_gap_of_reads_the_property() {
    assert_eq!(
        prior_object_id_gap_of(None),
        Ok(0),
        "Properties が無い Object の Prior Object ID Gap は 0 であること"
    );

    // Properties Length = 0 のみのブロック
    let empty = ObjectProperties::new();
    let mut empty_bytes = Vec::new();
    empty
        .encode(&mut empty_bytes)
        .expect("正当なテスト入力の encode は成功する");
    assert_eq!(
        prior_object_id_gap_of(Some(&empty_bytes)),
        Ok(0),
        "Property を持たない Object の Prior Object ID Gap は 0 であること"
    );

    let mut gap = ObjectProperties::new();
    gap.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(5),
    });
    let mut gap_bytes = Vec::new();
    gap.encode(&mut gap_bytes)
        .expect("正当なテスト入力の encode は成功する");
    assert_eq!(
        prior_object_id_gap_of(Some(&gap_bytes)),
        Ok(5),
        "PRIOR_OBJECT_ID_GAP の値を読むこと"
    );

    // IMMUTABLE_PROPERTIES の内側に置かれた場合も探索する (draft-ietf-moq-transport-22
    // §10.7 (Immutable Properties) の "MUST search both")
    let mut immutable = ObjectProperties::new();
    immutable.push(ObjectProperty {
        prop_type: PROP_IMMUTABLE_PROPERTIES,
        value: ObjectPropertyValue::Bytes(inner_prior_object_id_gap(4)),
    });
    let mut immutable_bytes = Vec::new();
    immutable
        .encode(&mut immutable_bytes)
        .expect("正当なテスト入力の encode は成功する");
    assert_eq!(
        prior_object_id_gap_of(Some(&immutable_bytes)),
        Ok(4),
        "IMMUTABLE_PROPERTIES の内側の PRIOR_OBJECT_ID_GAP も読むこと"
    );

    // 宣言長が実データ長を超えるブロック
    assert!(
        matches!(
            prior_object_id_gap_of(Some(b"\x05\x3e")),
            Err(MessageError::ProtocolViolation(_))
        ),
        "宣言長に対してデータが足りない Properties は書式違反として拒否すること"
    );
    // 空のバイト列 (Properties Length すら無い)
    assert!(
        matches!(
            prior_object_id_gap_of(Some(&[])),
            Err(MessageError::ProtocolViolation(_))
        ),
        "空の Properties は書式違反として拒否すること"
    );
    // 末尾に余分なバイトがあるブロック
    let mut extra_bytes = gap_bytes.clone();
    extra_bytes.push(0x00);
    assert!(
        matches!(
            prior_object_id_gap_of(Some(&extra_bytes)),
            Err(MessageError::ProtocolViolation(_))
        ),
        "宣言長と実データ長が一致しない Properties は書式違反として拒否すること"
    );
}

/// IMMUTABLE_PROPERTIES (0x0B) の値 (入れ子 KVP 列) を 1 つの varint プロパティで作る
///
/// `ObjectProperties` としてエンコードした結果から、先頭の Properties Length varint を
/// 除去して内側の KVP 列だけを取り出す。
fn inner_prior_object_id_gap(value: u64) -> Vec<u8> {
    let mut props = ObjectProperties::new();
    props.push(ObjectProperty {
        prop_type: PROP_PRIOR_OBJECT_ID_GAP,
        value: ObjectPropertyValue::VarInt(value),
    });
    let mut buf = Vec::new();
    props
        .encode(&mut buf)
        .expect("正当なテスト入力の encode は成功する");
    let (_, n) = varint::decode(&buf).expect("エンコードされたバッファは varint で始まる");
    buf[n..].to_vec()
}

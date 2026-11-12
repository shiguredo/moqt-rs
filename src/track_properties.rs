//! Track Properties (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties))
//!
//! Track Properties は KVP 形式 (Setup Options と同一)。
//! カウントプレフィックスなし、メッセージペイロードの残り全体を読む。
//! 偶数型は varint 値、奇数型は長さ付きバイト列。
//!
//! SUBSCRIBE_OK, PUBLISH, FETCH_OK のメッセージ末尾に含まれる。
//! Message Parameters の後に配置される.
//!
//! # 対象 Property (draft-ietf-moq-transport-23 §16.8 (Properties) Table 15)
//!
//! | Type | Name                          | Scope        | Spec |
//! |-----:|-------------------------------|--------------|------|
//! | 0x02 | OBJECT_DELIVERY_TIMEOUT       | Track,Object | draft-ietf-moq-transport-23 §10.2 (OBJECT_DELIVERY_TIMEOUT) |
//! | 0x04 | MAX_CACHE_DURATION            | Track        | draft-ietf-moq-transport-23 §10.3 (MAX CACHE DURATION) |
//! | 0x06 | SUBGROUP_DELIVERY_TIMEOUT     | Track,Object | draft-ietf-moq-transport-23 §10.1 (SUBGROUP_DELIVERY_TIMEOUT) |
//! | 0x0B | IMMUTABLE_PROPERTIES          | Track,Object | draft-ietf-moq-transport-23 §10.7 (Immutable Properties) |
//! | 0x0E | DEFAULT_PUBLISHER_PRIORITY    | Track        | draft-ietf-moq-transport-23 §10.4 (DEFAULT PUBLISHER PRIORITY) |
//! | 0x22 | DEFAULT_PUBLISHER_GROUP_ORDER | Track        | draft-ietf-moq-transport-23 §10.5 (DEFAULT PUBLISHER GROUP ORDER) |
//! | 0x30 | DYNAMIC_GROUPS                | Track        | draft-ietf-moq-transport-23 §10.6 (DYNAMIC GROUPS) |
//!
//! 書式は `object_properties` モジュールの対応表と揃えている。
//!
//! このほか draft-ietf-moq-loc-05 §6.1 (MOQ Properties Registry) Table 1 の
//! 0x11 CODEC_STRING (Scope=Track, Object) を扱う。値の UTF-8 検証は
//! `crate::property` の共有検証を使い、`TrackProperties::codec_string` から取得する。
//!
//! # スコープ検証と多重検証
//!
//! draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) は、この実装が意味を理解する
//! Property をスコープ外で受信した場合に Track を malformed とすることを MUST とする
//! (§12.1 (Malformed Tracks) 条件 8)。同じ節は、Property の定義が複数を許す場合を除いて
//! 1 インスタンスまでしか載せてはならないと定め、許される数より多く受信した場合も Track を
//! malformed とすることを MUST とする (§12.1 条件 9)。構造 decode
//! (`TrackProperties::decode`) はどちらの検証も行わず、重複した Type をそのまま保持する。
//! Request ID と Full Track Name を持つ `Session` の受信ハンドラが
//! [`crate::track_properties::TrackProperties::validate`] で検証する (構造と意味の分離)。
//! ローカル入力の検証である `encode` は、受信 Track の判定と混同しないよう
//! `ProtocolViolation` で拒否する。
// CODEC_STRING (0x11) は LOC が定義する Property だが、Scope が Track と Object の両方で
// あるため transport 側の TrackProperties でも UTF-8 を検証する。Property Type と検証規則は
// 共有のため `crate::property` に置いており、このモジュールは `crate::loc` の型・関数を
// 直接 import しない (LOC の定数はスコープ表 `crate::property_scope` 経由で参照する)。
use crate::property::{CODEC_STRING_TYPE as PROP_CODEC_STRING, validate_codec_string_utf8};
use crate::{error::MessageError, varint};
use alloc::vec::Vec;

/// OBJECT_DELIVERY_TIMEOUT (Property Type 0x02, draft-ietf-moq-transport-23 §10.2 (OBJECT_DELIVERY_TIMEOUT))
pub const PROP_OBJECT_DELIVERY_TIMEOUT: u64 = 0x02;
/// MAX_CACHE_DURATION (Property Type 0x04, draft-ietf-moq-transport-23 §10.3 (MAX CACHE DURATION))
///
/// この節番号・定義は draft-ietf-moq-transport-23 由来であり、将来の draft 改版で
/// 変わる可能性がある。
pub const PROP_MAX_CACHE_DURATION: u64 = 0x04;
/// SUBGROUP_DELIVERY_TIMEOUT (Property Type 0x06, draft-ietf-moq-transport-23 §10.1 (SUBGROUP_DELIVERY_TIMEOUT))
pub const PROP_SUBGROUP_DELIVERY_TIMEOUT: u64 = 0x06;
/// DEFAULT_PUBLISHER_PRIORITY (Property Type 0x0E)
pub const PROP_DEFAULT_PUBLISHER_PRIORITY: u64 = 0x0E;
/// DEFAULT_PUBLISHER_GROUP_ORDER (Property Type 0x22)
pub const PROP_DEFAULT_PUBLISHER_GROUP_ORDER: u64 = 0x22;
/// DYNAMIC_GROUPS (Property Type 0x30)
pub const PROP_DYNAMIC_GROUPS: u64 = 0x30;
/// IMMUTABLE_PROPERTIES (Property Type 0x0B, draft-ietf-moq-transport-23 §10.7 (Immutable Properties))
///
/// 奇数型だが値は「入れ子の Track Property Key-Value-Pair 列」。Original Publisher のみが
/// 付与でき、Relay は改変・削除してはならない。入れ子の IMMUTABLE_PROPERTIES は禁止
/// (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))。この節番号は draft-ietf-moq-transport-23 由来であり、将来の
/// draft 改版で変わる可能性がある。
pub const PROP_IMMUTABLE_PROPERTIES: u64 = 0x0B;

/// 必須トラックプロパティの範囲下限 (draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties))
pub const MANDATORY_TRACK_PROPERTY_MIN: u64 = 0x4000;
/// 必須トラックプロパティの範囲上限 (draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties))
pub const MANDATORY_TRACK_PROPERTY_MAX: u64 = 0x7FFF;

/// Track scope に Object 専用の既知 Property が現れたときの説明
///
/// 受信時の `MalformedTrack` とローカル入力検証時の `ProtocolViolation` が同じ理由を
/// 指すように 1 箇所に置く (どちらも `MessageError` の `&'static str` である)。
const OBJECT_ONLY_IN_TRACK_SCOPE: &str = "Object-only property in Track scope";

/// 単一に限られる既知 Property が複数現れたときの説明
///
/// 受信時の `MalformedTrack` とローカル入力検証時の `ProtocolViolation` が同じ理由を
/// 指すように 1 箇所に置く (どちらも `MessageError` の `&'static str` である)。
const DUPLICATE_TRACK_PROPERTY: &str = "duplicate track property";

/// 奇数型 KVP の宣言長が残りバイトを超えたときの説明
///
/// decode ではこの説明を持つ `MalformedKeyValue` を返す。encode の
/// IMMUTABLE_PROPERTIES 内側検証は `MalformedKeyValue` をまとめて `ProtocolViolation` に
/// 写すため、この理由文字列でエラーを判別しない。CODEC_STRING (0x11) の不正 UTF-8 は
/// 別経路で検証し、`MalformedKeyValue` のまま返す。
const LENGTH_EXCEEDS_BUFFER: &str = "track property value length exceeds remaining buffer";

/// Track Property の値
///
/// - 偶数型: varint 値
/// - 奇数型: 長さ付きバイト列 (最大 65535 バイト)
#[derive(Debug, Clone, PartialEq)]
pub enum TrackPropertyValue {
    /// 偶数型の値 (varint)
    VarInt(u64),
    /// 奇数型の値 (バイト列、最大 65535 バイト)
    Bytes(Vec<u8>),
}

/// 単一の Track Property
#[derive(Debug, Clone, PartialEq)]
pub struct TrackProperty {
    /// プロパティ型 (draft-ietf-moq-transport-23 §8.4)
    pub prop_type: u64,
    /// プロパティ値 (偶数型は varint、奇数型はバイト列)
    pub value: TrackPropertyValue,
}

/// Track Properties リスト (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties))
///
/// カウントプレフィックスなし。エンコード時に `prop_type` の昇順にソートして
/// デルタエンコードを適用する。空の場合は 0 バイトにエンコードされる。
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackProperties(Vec<TrackProperty>);

impl TrackProperties {
    /// 空のコレクションを作成する
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// プロパティを末尾に追加する
    pub fn push(&mut self, p: TrackProperty) {
        self.0.push(p);
    }

    /// プロパティ数を返す
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// コレクションが空かどうかを返す
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 内部のプロパティスライスを返す
    pub fn as_slice(&self) -> &[TrackProperty] {
        &self.0
    }

    /// Track Properties をバッファにエンコードする (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties))
    ///
    /// カウントプレフィックスなし。デルタエンコードされた KVP を直接書き出す。
    /// 空の場合は何も書き出さない (0 バイト)。
    ///
    /// 未知の Type は複数インスタンスを保持したまま書き出す (draft-ietf-moq-transport-23
    /// §8.4 (Track and Object Properties) の "MUST allow multiple instances of the
    /// Property")。同一 Type の並び順はソートが安定であるため入力順を保つ。
    ///
    /// # Errors
    ///
    /// - 単一に限られる既知 Type の重複 (IMMUTABLE_PROPERTIES の内側との重複を含む):
    ///   `ProtocolViolation` (ローカル入力の検証であり、受信 Track の malformed 判定とは別)
    /// - 型と値形式の不一致: `InvalidParameter`
    /// - 既知 Type の値域違反 (DYNAMIC_GROUPS が 0/1 以外など): `MalformedKeyValue`
    /// - CODEC_STRING (0x11) の値が不正な UTF-8: `MalformedKeyValue`
    ///   (mutable リストと、解析できる IMMUTABLE_PROPERTIES の内側)
    /// - Object 専用の既知 Type (PRIOR_GROUP_ID_GAP など) を Track scope に含む:
    ///   `ProtocolViolation` (ローカル入力の検証であり、受信 Track の malformed 判定とは別)
    /// - トップレベルのバイト列が 65535 バイトを超える: `FieldLengthExceeded`
    ///   (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の値長上限 2^16-1)
    /// - IMMUTABLE_PROPERTIES の内側 KVP が不正 (入れ子の 0x0B、宣言長が残りバイト超過、
    ///   既知 Type の値域違反など): `ProtocolViolation` (decode 層の `MalformedKeyValue` を
    ///   `ProtocolViolation` に写す。ただし CODEC_STRING の不正 UTF-8 は値の書式違反として
    ///   `MalformedKeyValue` のまま返す)
    /// - IMMUTABLE_PROPERTIES の内側 KVP の varint が途中終端: `UnexpectedEof`
    ///   (呼び出し側が渡した直列化の切り詰めであり、透過する)
    /// - IMMUTABLE_PROPERTIES の内側 KVP の宣言長が 65535 バイトを超える:
    ///   `FieldLengthExceeded` / 内側の delta-key のオーバーフロー: `DeltaEncodingOverflow`
    ///   (内側の走査エラーをそのまま透過する)
    pub fn encode(&self, buf: &mut Vec<u8>) -> Result<(), MessageError> {
        let mut sorted = self.0.clone();
        sorted.sort_by_key(|p| p.prop_type);

        // ローカル入力の多重可否を検証する (draft-ietf-moq-transport-23 §8.4 (Track and
        // Object Properties) / §12.1 (Malformed Tracks) 条件 9)。IMMUTABLE_PROPERTIES の
        // 内側も合わせて数える (§10.7 (Immutable Properties) の "MUST search both")。
        // 受信 Track の malformed 判定と混同せず ProtocolViolation で拒否する。
        if self.has_duplicate_single_property() {
            return Err(MessageError::ProtocolViolation(DUPLICATE_TRACK_PROPERTY));
        }

        let mut prev_type: u64 = 0;
        for prop in &sorted {
            // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): 偶数型は varint、奇数型は長さ付きバイト列
            // 型と値形式の整合性を検証する
            match (&prop.value, prop.prop_type % 2) {
                (TrackPropertyValue::VarInt(_), 0) => {}
                (TrackPropertyValue::Bytes(_), 1) => {}
                _ => return Err(MessageError::InvalidParameter),
            }
            // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): Object 専用の
            // 既知 Property を Track scope に載せた直列化は生成しない。encode は受信 Track の
            // 判定ではなくローカル入力の検証であるため、MalformedTrack ではなく
            // ProtocolViolation で拒否する (§12.1 (Malformed Tracks) の cancel を起こさない)。
            if crate::property_scope::is_object_only(prop.prop_type) {
                return Err(MessageError::ProtocolViolation(OBJECT_ONLY_IN_TRACK_SCOPE));
            }
            // draft-ietf-moq-transport-23 §10.4 (DEFAULT PUBLISHER PRIORITY) / draft-ietf-moq-transport-23 §10.5 (DEFAULT PUBLISHER GROUP ORDER) / draft-ietf-moq-transport-23 §10.6 (DYNAMIC GROUPS): 既知 Track Property の値域を検証する
            let varint_value = match &prop.value {
                TrackPropertyValue::VarInt(v) => Some(*v),
                TrackPropertyValue::Bytes(_) => None,
            };
            validate_track_property_value_range(prop.prop_type, varint_value)?;
            crate::kvp::encode_delta_key(prev_type, prop.prop_type, buf);
            match &prop.value {
                TrackPropertyValue::VarInt(v) => {
                    varint::encode(*v, buf);
                }
                TrackPropertyValue::Bytes(bytes) => {
                    // draft-ietf-moq-transport-23 §8.4 (Track and Object Properties):
                    // 奇数型の値長上限は 2^16-1 バイト。object_properties / loc と同様に
                    // encode 側でも検証し、上限超過のワイヤーフォーマット生成を防ぐ。
                    if bytes.len() > 65535 {
                        return Err(MessageError::FieldLengthExceeded(
                            "track property value length exceeds 65535",
                        ));
                    }
                    if prop.prop_type == PROP_CODEC_STRING {
                        // draft-ietf-moq-loc-05 §2.3.1.3 (Codec String) の値を UTF-8 として
                        // 検証する。不正な UTF-8 は draft-ietf-moq-transport-23 §8.3
                        // (Key-Value-Pair Structure) の MALFORMED_KEY_VALUE に対応する。
                        validate_codec_string_utf8(bytes)?;
                    }
                    // draft-ietf-moq-transport-23 §10.7 (Immutable Properties): IMMUTABLE_PROPERTIES の内側を Track Property KVP 列として
                    // 検証する (入れ子の 0x0B を拒否)。検証のみで、バイト列はそのまま再出力する
                    // (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の "the serialization ... of
                    // the Key-Value-Pairs MUST NOT change")。
                    if prop.prop_type == PROP_IMMUTABLE_PROPERTIES {
                        // 内側の構造・値域を CODEC_STRING の UTF-8 検証なしで借用走査する。
                        // ローカル入力の検証である encode では、この走査で返る
                        // MalformedKeyValue (宣言長超過・既知 Type の値域違反など) を、既存の
                        // encode 検証 (重複・2^16-1 超) と同じ ProtocolViolation に揃える。
                        // 入れ子の 0x0B など ProtocolViolation 系はそのまま透過させる。
                        let inner = match walk_track_kv_pairs(bytes, false, false) {
                            Ok(inner) => inner,
                            Err(err) => {
                                return Err(match err {
                                    MessageError::MalformedKeyValue(_) => {
                                        MessageError::ProtocolViolation(
                                            "immutable properties inner KVP is malformed",
                                        )
                                    }
                                    other => other,
                                });
                            }
                        };
                        // 内側も mutable リストと同じスコープ規則で検証する
                        // (draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の
                        // "Properties MAY appear either in the mutable property list or
                        // inside Immutable Properties")。入力の並び順でエラー分類が
                        // 変わらないよう、スコープ検証を UTF-8 検証より先に行う。
                        for inner_prop in &inner {
                            if crate::property_scope::is_object_only(inner_prop.prop_type) {
                                return Err(MessageError::ProtocolViolation(
                                    OBJECT_ONLY_IN_TRACK_SCOPE,
                                ));
                            }
                        }
                        for inner_prop in &inner {
                            // CODEC_STRING (0x11) の UTF-8 を検証する。これは値の書式違反
                            // であるため draft-ietf-moq-transport-23 §8.3
                            // (Key-Value-Pair Structure) の MALFORMED_KEY_VALUE に対応する
                            // MalformedKeyValue のまま返す。
                            if inner_prop.prop_type == PROP_CODEC_STRING
                                && let TrackPropertyValueRef::Bytes(value) = inner_prop.value
                            {
                                validate_codec_string_utf8(value)?;
                            }
                        }
                    }
                    varint::encode(bytes.len() as u64, buf);
                    buf.extend_from_slice(bytes);
                }
            }
            prev_type = prop.prop_type;
        }
        Ok(())
    }

    /// バッファ全体から Track Properties をデコードする
    ///
    /// カウントプレフィックスなし。バッファ末尾まで KVP を読む。
    /// 空バッファの場合は空の `TrackProperties` を返す。
    /// IMMUTABLE_PROPERTIES (0x0B) は内側を Track Property KVP 列として検証する (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))。
    ///
    /// 同一 Type の複数インスタンスは構造として保持する。未知の Type の複数は draft-ietf-moq-transport-23
    /// §8.4 (Track and Object Properties) の "MUST allow multiple instances of the Property"
    /// により正当であり、既知の Type の複数は §12.1 (Malformed Tracks) 条件 9 の malformed
    /// Track 判定になる。どちらも Request ID と Full Track Name を持つ受信ハンドラが
    /// [`TrackProperties::validate`] で判定する (構造と意味の分離)。
    ///
    /// # Errors
    ///
    /// - delta key / 偶数 Type の値 / 奇数 Type の Length の varint 途中終端: `UnexpectedEof`
    /// - 奇数 Type の宣言長が 2^16-1 超: `FieldLengthExceeded`
    /// - 奇数 Type の宣言長が 2^16-1 以下で残りバイト超: `MalformedKeyValue`
    ///   (既知 Type は draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の MUST。
    ///   未知 Type も §16.8 (Properties) の length による skip が成立しないため同じ分類に
    ///   揃える)
    /// - KVP Type の delta overflow / 入れ子の IMMUTABLE_PROPERTIES:
    ///   `ProtocolViolation`
    /// - 既知 Type の値域違反 (DYNAMIC_GROUPS が 0/1 以外など): `MalformedKeyValue`
    /// - CODEC_STRING (0x11) の値が不正な UTF-8: `MalformedKeyValue`
    ///   (mutable リストと IMMUTABLE_PROPERTIES の内側)
    ///
    /// **スコープ違反と多重違反は検証しない。** draft-ietf-moq-transport-23 §8.4 (Track and
    /// Object Properties) / §12.1 (Malformed Tracks) の条件 8 / 9 は構造ではなく意味の検証で
    /// あり、§12.1 の cancel には Request ID と Full Track Name が要る。decode 層はどちらも
    /// 持たないため、[`TrackProperties::validate`] を Request ID と Full Track Name を持つ
    /// `Session` の受信ハンドラが呼ぶ (構造と意味の分離)。
    pub fn decode(buf: &[u8]) -> Result<Self, MessageError> {
        Ok(Self(decode_track_kv_pairs(buf, true, true)?))
    }

    /// Track Properties に Object 専用の既知 Property が含まれていないかを検証する
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): "An endpoint that
    /// supports a Property and receives it outside its scope (e.g. a Track-only Property as an
    /// Object Property) MUST treat the Track as malformed" / §12.1 (Malformed Tracks) 条件 8。
    /// mutable リストと IMMUTABLE_PROPERTIES (0x0B) の内側の両方を調べる
    /// (§10.7 (Immutable Properties) の "MUST search both")。
    ///
    /// 既知 Property のスコープは内部モジュール `crate::property_scope` の表で判定する。未知の型と
    /// GREASE 値はスコープを推測できないため拒否しない (§8.4 の転送 MUST / §16.8 (Properties)
    /// の "Endpoints MUST ignore unknown Property types")。
    ///
    /// 本メソッドは受信した Track の判定に使う。ローカル入力の `encode` は受信 Track の取消と
    /// 混同しないよう `ProtocolViolation` で拒否する。
    ///
    /// `push` で積んだ IMMUTABLE_PROPERTIES の内側が破損している場合、内側は探索できない
    /// ([`TrackProperties::find_varint`] と同じ制約)。wire 経由の値は `decode` が内側の
    /// 書式を検証済みなので、この制約が観測されるのは in-memory 構築の入力だけである。
    ///
    /// # Errors
    ///
    /// Object 専用の既知 Property を含む場合に `MalformedTrack`。
    pub fn validate_scope(&self) -> Result<(), MessageError> {
        let inner = self.immutable_inner_properties();
        for p in self.0.iter().chain(inner.iter()) {
            if crate::property_scope::is_object_only(p.prop_type) {
                return Err(MessageError::MalformedTrack(OBJECT_ONLY_IN_TRACK_SCOPE));
            }
        }
        Ok(())
    }

    /// Track Properties に単一に限られる既知 Property の重複が無いかを検証する
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties): "A Track or Object MUST
    /// NOT contain more than one instance of the Property." (Property の定義が複数を許す場合を
    /// 除く) / §12.1 (Malformed Tracks) 条件 9。mutable リストと IMMUTABLE_PROPERTIES (0x0B)
    /// の内側を合わせて数える (§10.7 (Immutable Properties) の "MUST search both"。同節は
    /// 複数の値を許す Property に限り両方への出現を許す)。
    ///
    /// 未知の Type と GREASE 値は複数インスタンスを許す (§8.4 の "MUST allow multiple
    /// instances of the Property")。判定の詳細は `crate::property_scope` を参照。
    ///
    /// 本メソッドは受信した Track の判定に使う。ローカル入力の `encode` は受信 Track の取消と
    /// 混同しないよう `ProtocolViolation` で拒否する。
    ///
    /// `push` で積んだ IMMUTABLE_PROPERTIES の内側が破損している場合、内側は数えられない
    /// ([`TrackProperties::find_varint`] と同じ制約)。wire 経由の値は `decode` が内側の書式を
    /// 検証済みなので、この制約が観測されるのは in-memory 構築の入力だけである。
    ///
    /// # Errors
    ///
    /// 単一に限られる既知 Property が 2 インスタンス以上ある場合に `MalformedTrack`。
    pub fn validate_multiplicity(&self) -> Result<(), MessageError> {
        if self.has_duplicate_single_property() {
            return Err(MessageError::MalformedTrack(DUPLICATE_TRACK_PROPERTY));
        }
        Ok(())
    }

    /// Track Properties の意味検証 (スコープと多重可否) を行う
    ///
    /// draft-ietf-moq-transport-23 §8.4 (Track and Object Properties) / §12.1 (Malformed
    /// Tracks) 条件 8 / 9 の検証をまとめて行う。Request ID と Full Track Name を持つ
    /// `Session` の受信ハンドラが、受信した Track Properties に対して呼ぶ。違反は
    /// `MalformedTrack` として返り、呼び出し側が同じ Track の subscription / FETCH の cancel
    /// とアプリへの通知へ変換する。順序はスコープ検証が先である (どちらも同じ Track の
    /// malformed であり、返す理由だけが異なる)。
    ///
    /// # Errors
    ///
    /// [`TrackProperties::validate_scope`] または [`TrackProperties::validate_multiplicity`]
    /// が返す `MalformedTrack`。
    pub fn validate(&self) -> Result<(), MessageError> {
        self.validate_scope()?;
        self.validate_multiplicity()
    }

    /// mutable リストと IMMUTABLE_PROPERTIES の内側を合わせて、単一に限られる既知 Property が
    /// 重複しているかを返す
    fn has_duplicate_single_property(&self) -> bool {
        let inner = self.immutable_inner_properties();
        crate::property_scope::has_duplicate_single_property(
            self.0.iter().chain(inner.iter()).map(|p| p.prop_type),
        )
    }

    /// 偶数型 Track Property から varint 値を取得する
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties)「MUST search both」に従い、
    /// mutable リストを先に、無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    ///
    /// prop_type と値形式の両方が一致する最初のインスタンスを返す。wire 経由の値は
    /// [`TrackProperties::decode`] が偶数型を varint として読むため値形式は常に一致し、
    /// 不一致が観測されるのは型と値を手で組み立てた in-memory 入力だけである。
    ///
    /// 本メソッド自体は多重検証を行わず、同じ型が複数インスタンスある場合は最初のものを返す。
    /// 単一に限られる既知の型の重複は [`TrackProperties::validate_multiplicity`] が malformed
    /// Track として拒否するが、それは `Session` の受信経路が `decode` の後に `validate` を
    /// 呼ぶことによるものであり、本メソッドの前提ではない。
    ///
    /// この探索規則は draft 由来であり、将来の draft 改版で変わる可能性がある。
    pub fn find_varint(&self, prop_type: u64) -> Option<u64> {
        match self.find_property(prop_type, FindValue::VarInt)? {
            TrackPropertyValueRef::VarInt(v) => Some(v),
            TrackPropertyValueRef::Bytes(_) => None,
        }
    }

    /// 奇数型 Track Property から生バイト列を取得する
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties)「MUST search both」に従い、
    /// mutable リストを先に、無ければ IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。内側から
    /// 取得する場合も外側の `Bytes` を借用したまま返すため、値のバイト列を `Vec` にコピーしない。
    ///
    /// draft-ietf-moq-loc-05 §6.1 (MOQ Properties Registry) Table 1 は 0x0D (VIDEO_CONFIG) /
    /// 0x0F (AUDIO_CONFIG) / 0x11 (CODEC_STRING) の Scope を "Track, Object" と定める。
    /// draft-ietf-moq-loc-05 §3.1.2 (Immutable Properties) は、relay から見える不変の LOC
    /// プロパティを IMMUTABLE_PROPERTIES として符号化することを SHOULD とする。そのため
    /// Track スコープのこれらの値は、最上位リストと IMMUTABLE_PROPERTIES の内側のどちらに
    /// 置かれても本メソッドで取り出せなければならない。LOC に限らず任意の奇数型を扱う。
    ///
    /// prop_type と値形式の両方が一致する最初のインスタンスを返す。本メソッド自体は多重検証を
    /// 行わない点は [`TrackProperties::find_varint`] と同じである。
    ///
    /// `push` で積んだ IMMUTABLE_PROPERTIES の内側が破損していてデコードできない場合は
    /// `None` を返す ([`TrackProperties::find_varint`] と同じ制約)。wire 経由の値は
    /// [`TrackProperties::decode`] が内側の書式を検証済みなので、この制約が観測されるのは
    /// in-memory 構築の入力だけである。
    ///
    /// CODEC_STRING (0x11) の値を文字列として取得する場合は、UTF-8 の検証を行う
    /// [`TrackProperties::codec_string`] を使う。本メソッドは生バイト列を返すため、不正な
    /// UTF-8 もそのまま返す。
    ///
    /// この探索規則と LOC の節番号は draft 由来であり、将来の draft 改版で変わる可能性がある。
    pub fn find_bytes(&self, prop_type: u64) -> Option<&[u8]> {
        match self.find_property(prop_type, FindValue::Bytes)? {
            TrackPropertyValueRef::VarInt(_) => None,
            TrackPropertyValueRef::Bytes(b) => Some(b),
        }
    }

    /// Codec String (Property Type 0x11) を UTF-8 文字列として返す
    ///
    /// draft-ietf-moq-loc-05 §2.3.1.3 (Codec String) / §6.1 (MOQ Properties Registry)
    /// Table 1 (Scope=Track, Object)。WebCodecs Codec Registry が定義する codec 値であり、
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties) の「MUST search both」に
    /// 従って mutable リストと IMMUTABLE_PROPERTIES (0x0B) の内側を探索する。
    ///
    /// 欠落は `Ok(None)`、値が不正な UTF-8 の場合は `MalformedKeyValue` を返す
    /// (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure) の
    /// MALFORMED_KEY_VALUE)。欠落と不正値を区別できるよう `Option` だけでは表現しない。
    ///
    /// 内側の探索は構造・値域・長さを検証する (スコープと多重可否は
    /// [`TrackProperties::validate`] が行う)。UTF-8 は本メソッドが値を文字列として
    /// 取り出すときに検証する。そのため `push` で積んだ IMMUTABLE_PROPERTIES の内側に
    /// 不正な UTF-8 があり、かつ mutable リストに 0x11 が無い場合は `MalformedKeyValue` に
    /// なる。内側の走査が失敗した場合 (構造破損・値域違反・長さ超過・入れ子の 0x0B など) は
    /// [`TrackProperties::find_bytes`] が `None` を返し、本メソッドも `Ok(None)` になる。
    /// wire 経由の値は [`TrackProperties::decode`] が不正な UTF-8 を拒否するため、この
    /// 区別が観測されるのは in-memory 構築の入力だけである。
    pub fn codec_string(&self) -> Result<Option<&str>, MessageError> {
        match self.find_bytes(PROP_CODEC_STRING) {
            Some(bytes) => validate_codec_string_utf8(bytes).map(Some),
            None => Ok(None),
        }
    }

    /// mutable リストと IMMUTABLE_PROPERTIES の内側から、prop_type と値形式が一致する最初の
    /// インスタンスを探す
    ///
    /// draft-ietf-moq-transport-23 §10.7 (Immutable Properties)「MUST search both」の
    /// 共通探索であり、[`TrackProperties::find_varint`] / [`TrackProperties::find_bytes`] は
    /// `wanted` を変えて呼ぶだけである。mutable リストを優先し、型が一致しても値形式が
    /// 一致しないインスタンスは読み飛ばす。IMMUTABLE_PROPERTIES の内側が破損している場合と
    /// 多重検証を行わない点の扱いは、公開メソッド側の doc を参照。
    fn find_property(
        &self,
        prop_type: u64,
        wanted: FindValue,
    ) -> Option<TrackPropertyValueRef<'_>> {
        // mutable リストを先に探索する (mutable 優先)
        for p in &self.0 {
            if p.prop_type == prop_type {
                let value = match &p.value {
                    TrackPropertyValue::VarInt(v) => TrackPropertyValueRef::VarInt(*v),
                    TrackPropertyValue::Bytes(b) => TrackPropertyValueRef::Bytes(b.as_slice()),
                };
                if value.matches(wanted) {
                    return Some(value);
                }
            }
        }
        // IMMUTABLE_PROPERTIES の内側を探索する。内側の値は外側の Bytes への借用のまま返す。
        let inner = self.immutable_properties()?;
        let props = walk_track_kv_pairs(inner, false, false).ok()?;
        props
            .into_iter()
            .find(|p| p.prop_type == prop_type && p.value.matches(wanted))
            .map(|p| p.value)
    }

    /// DYNAMIC_GROUPS (Property Type 0x30) の値を返す
    ///
    /// draft-ietf-moq-transport-23 §10.6 (DYNAMIC GROUPS): 0 または 1 のみ。`1` は dynamic groups を
    /// サポートすることを示す。parameter が含まれない場合は `None`。
    pub fn dynamic_groups(&self) -> Option<u64> {
        self.find_varint(PROP_DYNAMIC_GROUPS)
    }

    /// DEFAULT_PUBLISHER_PRIORITY (Property Type 0x0E) の値を返す
    ///
    /// draft-ietf-moq-transport-23 §10.4 (DEFAULT PUBLISHER PRIORITY): 0-255。
    /// "Subgroups and Datagrams for this subscription inherit this priority, unless they
    /// specifically override it." / "If omitted, the Default Publisher Priority is 128."
    ///
    /// 省略時の 128 は適用せず `None` を返す。デフォルト適用は「宣言されていない」ことを
    /// 区別できる呼び出し側 (Session) の責務とする。`u8` に収まらない値は
    /// `push` による in-memory 構築でのみ起こりうるため、その場合も `None` を返す。
    /// 節番号・規則は draft 由来であり将来 draft 改定で変わる可能性がある。
    pub fn default_publisher_priority(&self) -> Option<u8> {
        self.find_varint(PROP_DEFAULT_PUBLISHER_PRIORITY)
            .and_then(|v| u8::try_from(v).ok())
    }

    /// DEFAULT_PUBLISHER_GROUP_ORDER (Property Type 0x22) の値を返す
    ///
    /// draft-ietf-moq-transport-23 §10.5 (DEFAULT PUBLISHER GROUP ORDER): Ascending (0x1) または
    /// Descending (0x2)。"If omitted, the publisher's preference is Ascending (0x1)."
    ///
    /// これは **publisher の選好** であり、§9.19.8 (GROUP ORDER Parameter) が運ぶ
    /// subscriber からの要求 (`Subscription::group_order`) とは別の値である。
    /// 省略時の Ascending は適用せず `None` を返す。`u8` に収まらない値は
    /// `push` による in-memory 構築でのみ起こりうるため、その場合も `None` を返す。
    pub fn default_publisher_group_order(&self) -> Option<u8> {
        self.find_varint(PROP_DEFAULT_PUBLISHER_GROUP_ORDER)
            .and_then(|v| u8::try_from(v).ok())
    }

    /// DELIVERY_TIMEOUT (Property Type 0x02) の値を返す
    pub fn object_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PROP_OBJECT_DELIVERY_TIMEOUT)
    }

    /// SUBGROUP_DELIVERY_TIMEOUT (Property Type 0x06) の値を返す (draft-ietf-moq-transport-23 §10.1)
    pub fn subgroup_delivery_timeout(&self) -> Option<u64> {
        self.find_varint(PROP_SUBGROUP_DELIVERY_TIMEOUT)
    }

    /// prop_type が既知の必須プロパティかどうかを返す (draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties))
    ///
    /// 0x4000-0x7FFF 範囲のうち、この実装が認識しているプロパティ型を返す。
    /// draft-ietf-moq-transport-23 時点ではこの範囲に定義済みのプロパティは存在しない。
    fn is_known_mandatory(_prop_type: u64) -> bool {
        // draft-ietf-moq-transport-23 時点では 0x4000-0x7FFF 範囲の既知プロパティは定義されていない
        false
    }

    /// 未知の Mandatory Track Property の型を昇順・重複なしで列挙する
    ///
    /// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) の
    /// 0x4000-0x7FFF のうち、この実装が認識していない型を返す。draft-ietf-moq-transport-23
    /// §10.7 (Immutable Properties) の "MUST search both" に従い、mutable リストに加えて
    /// IMMUTABLE_PROPERTIES (0x0B) の内側も探索する。
    ///
    /// ただし §16.8 (Properties) の Table 15 が GREASE の Property Type
    /// (`0x7f * N + 0x9D`) を Scope Any として予約しており、その一部 (N = 128 の 0x401D
    /// から N = 256 の 0x7F9D まで) はこの範囲に入る。GREASE 値は IANA に登録された
    /// Property ではないため、§3.7 の「Mandatory Track Property」は登録された必須トラック
    /// プロパティを指し、予約値である GREASE を含まないと解釈する (§3.7 の字面は範囲全体を
    /// Mandatory とするため衝突するが、Table 15 が GREASE の Scope を Any と定めていることを
    /// 優先する)。この解釈は §13 (Grease) の
    /// "Endpoints MUST NOT close the session solely because they received an unknown value."
    /// と §16.8 (Properties) の "Endpoints MUST ignore unknown Property types, skipping them
    /// according to the Key-Value-Pair encoding" に整合する。
    /// Object scope の同じ衝突 (`crate::object_properties`) も同じ解釈で扱う。
    /// draft 内部の登録ポリシーと予約値の関係は将来の draft 改版で変わりうる。
    ///
    /// 返した型は REQUEST_ERROR (UNSUPPORTED_EXTENSION) の UNSUPPORTED_PROPERTY Error Detail
    /// (draft-ietf-moq-transport-23 §9.4.2.2 (UNSUPPORTED_PROPERTY)) に 1 型 1 Detail で載せる。
    /// mutable リストと IMMUTABLE_PROPERTIES の内側に同じ型が現れても Detail は 1 つで足りる
    /// ため重複は除去し、ワイヤ上の Detail の並び (型の昇順) に揃えて昇順にする。
    pub fn unknown_mandatory_types(&self) -> Vec<u64> {
        let inner = self.immutable_inner_properties();
        let mut types: Vec<u64> = self
            .0
            .iter()
            .chain(inner.iter())
            .map(|p| p.prop_type)
            .filter(|prop_type| Self::is_unknown_mandatory(*prop_type))
            .collect();
        types.sort_unstable();
        types.dedup();
        types
    }

    /// 未知の必須プロパティを含むかどうかを返す
    ///
    /// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties): 未知の
    /// 必須プロパティを受信したエンドポイントは PUBLISH なら REQUEST_ERROR
    /// (UNSUPPORTED_EXTENSION)、SUBSCRIBE_OK / FETCH_OK なら購読・ fetch のキャンセルを行う。
    /// 対象の判定は [`TrackProperties::unknown_mandatory_types`] と同じである。
    pub fn has_unknown_mandatory(&self) -> bool {
        !self.unknown_mandatory_types().is_empty()
    }

    /// prop_type が未知の Mandatory Track Property かを返す
    ///
    /// draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties) の範囲に
    /// 入り、この実装が認識せず (§16.8 (Properties) の既知型でなく)、GREASE 予約値でもない
    /// (§13 (Grease)) 型を未知の Mandatory Track Property とする。
    fn is_unknown_mandatory(prop_type: u64) -> bool {
        (MANDATORY_TRACK_PROPERTY_MIN..=MANDATORY_TRACK_PROPERTY_MAX).contains(&prop_type)
            && !Self::is_known_mandatory(prop_type)
            && !crate::grease::is_grease(prop_type)
    }

    /// IMMUTABLE_PROPERTIES (0x0B) の内側の生バイト列を返す
    ///
    /// 中身は入れ子の KVP 列 (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))。
    /// [`TrackProperties::find_bytes`] はこのバイト列を借用したまま内側の値のスライスを返す。
    ///
    /// 最初の IMMUTABLE_PROPERTIES (0x0B) のみを対象とする。0x0B は単一に限られるため、
    /// `decode` が保持した重複や `push` で積んだ重複は [`TrackProperties::validate_multiplicity`]
    /// が malformed Track として拒否する。その前提が破れた入力に対する防御として、先頭の
    /// 0x0B がバイト列でない場合も後続の 0x0B を探索せず `None` を返す。本メソッド自体は
    /// 多重検証を行わない点に注意すること。
    ///
    /// `ObjectProperties::immutable_properties` と同名だが、本メソッドは private であり、
    /// 外部に公開するのは型と値の解釈を持つ [`TrackProperties::find_varint`] /
    /// [`TrackProperties::find_bytes`] に限る (`TrackProperties` の公開 API を増やさない)。
    fn immutable_properties(&self) -> Option<&[u8]> {
        for p in &self.0 {
            if p.prop_type == PROP_IMMUTABLE_PROPERTIES {
                // 0x0B は単一に限られる。先頭の 0x0B がバイト列でない場合は前提が破れた入力で
                // あるため、後続の 0x0B を探索せず打ち切る。
                return match &p.value {
                    TrackPropertyValue::Bytes(b) => Some(b.as_slice()),
                    TrackPropertyValue::VarInt(_) => None,
                };
            }
        }
        None
    }

    /// IMMUTABLE_PROPERTIES (0x0B) の内側 Track Property 列をデコードして返す
    ///
    /// スコープ検証 ([`TrackProperties::validate_scope`])・多重可否の検証
    /// ([`TrackProperties::has_duplicate_single_property`])・未知の必須型の列挙
    /// ([`TrackProperties::unknown_mandatory_types`]) のために、mutable リストと内側を
    /// 合わせて列挙する。値の取得は借用走査の [`walk_track_kv_pairs`] を直接使う
    /// ([`TrackProperties::find_property`]) ため、本メソッドは値のコピーを伴ってよい。
    ///
    /// IMMUTABLE_PROPERTIES が無い場合や、内側が破損している (例: `push` で積んだ不正値)
    /// 場合は空 `Vec` を返す。decode 経由で構築された値の内側は decode 時に検証済みのため
    /// 妥当である。
    ///
    /// 最初の IMMUTABLE_PROPERTIES (0x0B) のみを対象とする理由は
    /// [`immutable_properties`](Self::immutable_properties) を参照。
    fn immutable_inner_properties(&self) -> Vec<TrackProperty> {
        let Some(bytes) = self.immutable_properties() else {
            return Vec::new();
        };
        // UTF-8 は encode / decode / codec_string が検証する。ここで検証すると、in-memory の
        // 不正 UTF-8 が内側全体を空にしてスコープ・多重・未知必須の診断まで落としてしまう。
        decode_track_kv_pairs(bytes, false, false).unwrap_or_default()
    }
}

/// 既知 Track Property の値域を検証する
/// (draft-ietf-moq-transport-23 §10.4 (DEFAULT PUBLISHER PRIORITY) /
/// §10.5 (DEFAULT PUBLISHER GROUP ORDER) / §10.6 (DYNAMIC GROUPS))
///
/// `varint_value` は偶数型の varint 値であり、奇数型 (バイト列) の場合は `None` を渡す。
/// 値域を持つ既知 Property はすべて偶数型であるため、`None` は検証対象外になる。
///
/// encode と decode (借用走査を含む) の両方から呼び、mutable リストと IMMUTABLE_PROPERTIES の
/// 内側に同一の値域検証を適用する。値域外の受信は同節の MUST により MALFORMED_KEY_VALUE で
/// 閉じる。
fn validate_track_property_value_range(
    prop_type: u64,
    varint_value: Option<u64>,
) -> Result<(), MessageError> {
    match (prop_type, varint_value) {
        // draft-ietf-moq-transport-23 §10.4 (DEFAULT PUBLISHER PRIORITY):
        // DEFAULT_PUBLISHER_PRIORITY は 0-255 のみ
        (PROP_DEFAULT_PUBLISHER_PRIORITY, Some(v)) if v > 255 => Err(
            MessageError::MalformedKeyValue("DEFAULT_PUBLISHER_PRIORITY must be 0..255"),
        ),
        // draft-ietf-moq-transport-23 §10.5 (DEFAULT PUBLISHER GROUP ORDER):
        // DEFAULT_PUBLISHER_GROUP_ORDER は 1 (Ascending) または 2 (Descending) のみ
        (PROP_DEFAULT_PUBLISHER_GROUP_ORDER, Some(v)) if v != 1 && v != 2 => {
            Err(MessageError::MalformedKeyValue(
                "DEFAULT_PUBLISHER_GROUP_ORDER must be 1 (Ascending) or 2 (Descending)",
            ))
        }
        // draft-ietf-moq-transport-23 §10.6 (DYNAMIC GROUPS): DYNAMIC_GROUPS は 0 または 1 のみ
        (PROP_DYNAMIC_GROUPS, Some(v)) if v > 1 => Err(MessageError::MalformedKeyValue(
            "DYNAMIC_GROUPS must be 0 or 1",
        )),
        _ => Ok(()),
    }
}

/// [`TrackProperties::find_property`] が探す値の形式
///
/// 偶数型は varint 値、奇数型は長さ付きバイト列という Property の値形式を表す。型と値形式の
/// 両方が一致するインスタンスだけを探索結果として扱うために使う。
#[derive(Debug, Clone, Copy)]
enum FindValue {
    /// 偶数型の varint 値
    VarInt,
    /// 奇数型の長さ付きバイト列
    Bytes,
}

/// 借用した Track Property 値
///
/// KVP 列の走査結果を保持する内部表現であり、奇数型の値は元のバッファへの借用のまま持つ。
/// [`TrackProperties::find_bytes`] が IMMUTABLE_PROPERTIES (0x0B) の内側の値を `Vec` へ
/// コピーせず返せるようにするために使う。
#[derive(Debug, Clone, Copy)]
enum TrackPropertyValueRef<'a> {
    /// 偶数型の値 (varint)
    VarInt(u64),
    /// 奇数型の値 (借用したバイト列)
    Bytes(&'a [u8]),
}

impl TrackPropertyValueRef<'_> {
    /// 偶数型の varint 値を返す
    ///
    /// 奇数型 (バイト列) の場合は `None`。借用走査中の値域検証
    /// ([`validate_track_property_value_range`]) に渡す表現へ正規化するために使う。
    fn as_varint(self) -> Option<u64> {
        match self {
            Self::VarInt(v) => Some(v),
            Self::Bytes(_) => None,
        }
    }

    /// 値の形式が `wanted` と一致するかを返す
    fn matches(self, wanted: FindValue) -> bool {
        matches!(
            (self, wanted),
            (Self::VarInt(_), FindValue::VarInt) | (Self::Bytes(_), FindValue::Bytes)
        )
    }
}

/// 借用した Track Property
///
/// [`walk_track_kv_pairs`] の走査結果を保持する内部表現であり、奇数型の値のバイト列を
/// コピーせず元のバッファへの借用のまま持つ。
#[derive(Debug, Clone, Copy)]
struct TrackPropertyRef<'a> {
    /// プロパティ型
    prop_type: u64,
    /// プロパティ値
    value: TrackPropertyValueRef<'a>,
}

impl TrackPropertyRef<'_> {
    /// 所有権を持つ [`TrackProperty`] へ変換する
    fn to_owned_property(self) -> TrackProperty {
        TrackProperty {
            prop_type: self.prop_type,
            value: match self.value {
                TrackPropertyValueRef::VarInt(v) => TrackPropertyValue::VarInt(v),
                TrackPropertyValueRef::Bytes(b) => TrackPropertyValue::Bytes(b.to_vec()),
            },
        }
    }
}

/// Track Property の Key-Value-Pair 列をバッファ全体からデコードする (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties))
///
/// カウントプレフィックスなしで `buf` 末尾まで読む。`TrackProperties::decode` 本体と、
/// スコープ・多重・未知必須の診断に使う統合検索 (`immutable_inner_properties`) から呼ばれる。
/// encode の IMMUTABLE_PROPERTIES (0x0B) 内側検証は、所有権変換を伴わない借用走査の
/// [`walk_track_kv_pairs`] を直接使う。
///
/// 奇数 Type の宣言長が 2^16-1 以下で残りバイトを超える場合は
/// `MessageError::MalformedKeyValue` を返す。既知 Type は draft-ietf-moq-transport-23
/// §8.3 (Key-Value-Pair Structure) の MUST であり、未知 Type も §16.8 (Properties) の
/// length による skip が成立しないため同じ分類に揃える。2^16-1 超は `FieldLengthExceeded`、
/// Length フィールドの varint 自体が途中で切れる場合は `UnexpectedEof` とする。
///
/// `allow_immutable` が false の場合、IMMUTABLE_PROPERTIES (0x0B) の出現を拒否する。入れ子の
/// IMMUTABLE_PROPERTIES は draft-ietf-moq-transport-23 §10.7 (Immutable Properties) により malformed である。なお同節の
/// malformed 条件文は "An Object contains ..." と Object を主語にするが、同節冒頭が
/// IMMUTABLE の中身を Track or Object Property と対称に定義することを根拠に、Track scope でも入れ子を
/// 禁止する。
///
/// Object scope (`object_properties::decode_kv_pairs`) と異なり、必須トラックプロパティ範囲
/// (0x4000-0x7FFF) は拒否しない。これらは Track scope では正当な必須プロパティであり、draft-ietf-moq-transport-23 §3.7 (Mandatory to Understand Track Properties)
/// は Object scope でのみ malformed と規定する (極性が逆転する)。
///
/// スコープ違反 (draft-ietf-moq-transport-23 §8.4 (Track and Object Properties)) と多重違反
/// (§12.1 (Malformed Tracks) 条件 9) もここでは検証しない。どちらも Track の malformed 判定で
/// あり、§12.1 の cancel には Request ID と Full Track Name が要る。decode 層はどちらも持たない
/// ため、意味検証は `Session` の受信ハンドラが [`TrackProperties::validate`] で行う (構造と
/// 意味の分離)。同一 Type の KVP はそのまま複数読み取る (§8.4 の "MUST allow multiple
/// instances of the Property" により未知の Type の複数は正当である)。
fn decode_track_kv_pairs(
    buf: &[u8],
    allow_immutable: bool,
    validate_codec_string: bool,
) -> Result<Vec<TrackProperty>, MessageError> {
    Ok(
        walk_track_kv_pairs(buf, allow_immutable, validate_codec_string)?
            .into_iter()
            .map(TrackPropertyRef::to_owned_property)
            .collect(),
    )
}

/// Track Property の Key-Value-Pair 列をバッファ全体から借用して走査する
///
/// [`decode_track_kv_pairs`] の走査本体であり、奇数型の値のバイト列を `Vec` へコピーせず元の
/// バッファへの借用として返す。[`TrackProperties::find_bytes`] が IMMUTABLE_PROPERTIES (0x0B)
/// の内側から生バイト列を借用したまま返すために使う。書式の検証とエラーの分類は
/// [`decode_track_kv_pairs`] と共通であり、専用のパーサーを別に持たない (CODEC_STRING の
/// 検証可否だけを引数で切り替える)。
///
/// `validate_codec_string` が true の場合、CODEC_STRING (0x11) の値の UTF-8 を検証する
/// (draft-ietf-moq-loc-05 §2.3.1.3 (Codec String) / draft-ietf-moq-transport-23 §8.3
/// (Key-Value-Pair Structure) の MALFORMED_KEY_VALUE)。[`TrackProperties::decode`] は true を
/// 渡す。encode の IMMUTABLE_PROPERTIES 内側検証は構造・値域だけを見るため false で走査し、
/// CODEC_STRING の UTF-8 はその走査結果に対して別途検証する。[`TrackProperties::find_bytes`]
/// の借用探索も false を渡す (構造と値域は検証するが、UTF-8 は
/// [`TrackProperties::codec_string`] が値を文字列として取り出すときに検証する)。
fn walk_track_kv_pairs<'a>(
    buf: &'a [u8],
    allow_immutable: bool,
    validate_codec_string: bool,
) -> Result<Vec<TrackPropertyRef<'a>>, MessageError> {
    let mut pos = 0;
    let mut props = Vec::new();
    let mut prev_type: u64 = 0;

    while pos < buf.len() {
        let (prop_type, _delta) = crate::kvp::decode_delta_key(prev_type, buf, &mut pos)?;

        // draft-ietf-moq-transport-23 §10.7 (Immutable Properties): 入れ子の IMMUTABLE_PROPERTIES は禁止
        if !allow_immutable && prop_type == PROP_IMMUTABLE_PROPERTIES {
            return Err(MessageError::ProtocolViolation(
                "nested IMMUTABLE_PROPERTIES is not allowed (draft-ietf-moq-transport-23 §10.7 (Immutable Properties))",
            ));
        }

        let value = if prop_type % 2 == 0 {
            // 偶数型: varint 値
            let (v, n) = varint::decode(&buf[pos..])?;
            pos += n;
            TrackPropertyValueRef::VarInt(v)
        } else {
            // 奇数型: 長さ付きバイト列
            let (len, n) = varint::decode(&buf[pos..])?;
            pos += n;
            // draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): 値長の上限は
            // 2^16-1 バイトであり、超過は FIELD_LENGTH_EXCEEDED (0x401) で閉じる (MUST)。
            // §12.2 (Session Termination Codes) の優先規則により、他の分類より先に判定する。
            if len > 65535 {
                return Err(MessageError::FieldLengthExceeded(
                    "track property value too long",
                ));
            }
            // draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure): 受信側が Type を
            // 理解する場合、Length/Value が定義と一致しないときは MALFORMED_KEY_VALUE
            // (0x6) でセッションを閉じる MUST。Length フィールド自体の varint 途中終端は
            // UnexpectedEof のまま (フレーミングの切り詰め) とし、宣言長が残りバイトを
            // 超える場合だけを書式違反として分類する。§8.3 の MUST は受信側が Type を
            // 理解する場合が条件だが、未知 Type も draft §16.8 (Properties) の length に
            // よる skip が成立しない点は同じため、同じ分類に揃える (本実装の方針)。
            // §10.7 (Immutable Properties) の malformed Track と重なる条件だが、decode 層は
            // Track / request を特定できず購読 cancel を発行できないため、§8.3 のセッション
            // 終了を優先する (本実装の判断であり、§10.7 への写像は別途対応する)。
            let len = varint::checked_len(len, buf[pos..].len())
                .map_err(|_| MessageError::MalformedKeyValue(LENGTH_EXCEEDS_BUFFER))?;
            let bytes = &buf[pos..pos + len];
            pos += len;

            // draft-ietf-moq-loc-05 §2.3.1.3 (Codec String): CODEC_STRING の値は UTF-8
            // 文字列である。受信側が Type を理解する場合、Length/Value が定義された
            // serialization と一致しなければ MALFORMED_KEY_VALUE でセッションを閉じる
            // (draft-ietf-moq-transport-23 §8.3 (Key-Value-Pair Structure))。Track scope の
            // 値は mutable リストと IMMUTABLE_PROPERTIES の内側のどちらにも置けるため、
            // walk_track_kv_pairs の 1 箇所で両方を検証する。
            if validate_codec_string && prop_type == PROP_CODEC_STRING {
                validate_codec_string_utf8(bytes)?;
            }

            // draft-ietf-moq-transport-23 §10.7 (Immutable Properties): IMMUTABLE_PROPERTIES 自体は許可するが、内側を Track Property KVP 列
            // として検証する (内側の入れ子 0x0B を拒否する)。空 (length=0) の内側は許容される。
            if prop_type == PROP_IMMUTABLE_PROPERTIES {
                walk_track_kv_pairs(bytes, false, validate_codec_string)?;
            }

            TrackPropertyValueRef::Bytes(bytes)
        };

        // draft-ietf-moq-transport-23 §10.4 (DEFAULT PUBLISHER PRIORITY) / draft-ietf-moq-transport-23 §10.5 (DEFAULT PUBLISHER GROUP ORDER) / draft-ietf-moq-transport-23 §10.6 (DYNAMIC GROUPS): 既知 Track Property の値域を検証する
        validate_track_property_value_range(prop_type, value.as_varint())?;

        props.push(TrackPropertyRef { prop_type, value });
        prev_type = prop_type;
    }

    Ok(props)
}

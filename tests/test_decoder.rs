//! `MessageDecoder` の単体テスト
//!
//! バッファ付きインクリメンタルデコーダーの境界条件・エラーパスを検証する。
//! Length は vi64 (1〜9 バイト) であり、非最小符号化も受理する
//! (draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers))。
use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::parameter::{DEFAULT_MAX_CONTROL_MESSAGE_SIZE, SetupOption, SetupOptionValue};
use shiguredo_moqt::{message::Setup, parameter::SetupOptions, varint};

/// 制御メッセージの受信上限を既定値にしたデコーダーを作る
fn decoder() -> MessageDecoder {
    MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
}

/// 本文長が `body_len` バイトになる SETUP メッセージを組み立てる
///
/// 本文長の境界条件 (受信上限ちょうど・超過) を検証するテスト用に、任意長の本文を持つ
/// 正当な SETUP を作る。SETUP の本文は Setup Options の並びであり、長さ付きバイト列の
/// オプション (奇数型) を 1 つ置くことで任意長に調整できる。
fn setup_frame_with_body_len(body_len: u64) -> Vec<u8> {
    // Setup Option の値長は 65,535 バイトまでである (§8.3 Key-Value-Pair Structure)。
    // それより長い本文は、型の異なるオプションに分割して作る。
    let max_value_len = 65_535usize;
    let mut value_lens = vec![(body_len as usize).min(max_value_len)];
    for _ in 0..32 {
        let mut options = SetupOptions::new();
        for (i, len) in value_lens.iter().enumerate() {
            options.push(SetupOption {
                // 奇数型は長さ付きバイト列であり、型をずらして重複を避ける
                option_type: 0x11 + (i as u64) * 2,
                value: SetupOptionValue::Bytes(vec![0u8; *len]),
            });
        }
        let encoded = ControlMessage::Setup(Setup { options })
            // 組み立て途中は目的長を超えることがあるため、十分な上限で encode する
            .encode(body_len + 65_536)
            .expect("任意長の本文を持つ SETUP を encode できること");
        // ヘッダは Type (0x2F00 は 2 バイト) + Length (vi64) である。vi64 は先頭の 1 の個数で
        // 符号化長を表す (0 個 = 1 バイト、n 個 = n+1 バイト)。
        let leading = encoded[2].leading_ones() as usize;
        let length_len = leading + 1;
        let actual = encoded.len() - 2 - length_len;
        if actual as u64 == body_len {
            return encoded;
        }
        if (actual as u64) < body_len {
            // 増やす: 末尾のオプションに足し、入りきらなければオプションを追加する
            let diff = (body_len - actual as u64) as usize;
            let last = value_lens.len() - 1;
            if value_lens[last] + diff <= max_value_len {
                value_lens[last] += diff;
            } else {
                let room = max_value_len - value_lens[last];
                value_lens[last] = max_value_len;
                value_lens.push(diff - room);
            }
        } else {
            // 減らす: 末尾から削る
            let mut diff = (actual as u64 - body_len) as usize;
            while diff > 0 {
                let last = value_lens.len() - 1;
                let take = diff.min(value_lens[last]);
                value_lens[last] -= take;
                diff -= take;
                if value_lens[last] == 0 && value_lens.len() > 1 {
                    value_lens.pop();
                } else {
                    break;
                }
            }
        }
    }
    panic!("本文長 {body_len} の SETUP を組み立てられなかった");
}

/// SETUP メッセージのエンコード済みバイト列を作る
fn setup_frame() -> Vec<u8> {
    ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    })
    .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
    .expect("正当なテスト入力の encode は成功する")
}

#[test]
fn test_decode_complete_message() {
    let encoded = setup_frame();

    let mut decoder = decoder();
    decoder.push(&encoded);

    let result = decoder
        .try_decode_message()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result.is_some());
    // デコード成功後はバッファが空になり、以降は None を返す
    assert!(
        decoder
            .try_decode_message()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none()
    );
}

#[test]
fn test_decode_insufficient_data() {
    let encoded = setup_frame();

    let mut decoder = decoder();
    // 途中までしかデータを入れない
    decoder.push(&encoded[..encoded.len() / 2]);

    let result = decoder
        .try_decode_message()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result.is_none());
}

#[test]
fn test_decode_incremental() {
    let encoded = setup_frame();

    let mut decoder = decoder();

    // 1 バイトずつ追加してデコードする
    for (i, &byte) in encoded.iter().enumerate() {
        decoder.push(&[byte]);
        let result = decoder
            .try_decode_message()
            .expect("テストフィクスチャの前提条件を満たす");
        if i < encoded.len() - 1 {
            assert!(result.is_none());
        } else {
            assert!(result.is_some());
        }
    }
}

#[test]
fn test_decode_empty_buffer() {
    let mut decoder = decoder();
    let result = decoder
        .try_decode_message()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result.is_none());
}

#[test]
fn test_decode_varint() {
    let mut buf = Vec::new();
    varint::encode(0x2F00, &mut buf);

    let mut decoder = decoder();
    decoder.push(&buf);

    let result = decoder
        .try_decode_varint()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(result, Some(0x2F00));
    // デコード成功後はバッファが空になり、以降は None を返す
    assert!(
        decoder
            .try_decode_varint()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none()
    );
}

#[test]
fn test_decode_varint_insufficient() {
    let mut buf = Vec::new();
    varint::encode(0x2F00, &mut buf);

    let mut decoder = decoder();
    // 1 バイトだけ入れる (2 バイト以上必要)
    decoder.push(&buf[..1]);

    let result = decoder
        .try_decode_varint()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result.is_none());

    // 残りを追加する
    decoder.push(&buf[1..]);
    let result = decoder
        .try_decode_varint()
        .expect("テストフィクスチャの前提条件を満たす");
    assert_eq!(result, Some(0x2F00));
}

#[test]
fn test_decode_multiple_messages() {
    let setup1 = ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    });
    let setup2 = ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    });
    let mut encoded = setup1
        .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
        .expect("正当なテスト入力の encode は成功する");
    encoded.extend(
        setup2
            .encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
            .expect("テストフィクスチャの前提条件を満たす"),
    );

    let mut decoder = decoder();
    decoder.push(&encoded);

    let result1 = decoder
        .try_decode_message()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result1.is_some());

    let result2 = decoder
        .try_decode_message()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result2.is_some());

    let result3 = decoder
        .try_decode_message()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result3.is_none());
}

/// 外側フレームが完全で本文が短い場合は MALFORMED_MESSAGE になる
///
/// draft-ietf-moq-transport-23 §9 (Control Messages): Length が Message Body と一致しなければ
/// MALFORMED_MESSAGE でセッションを閉じる (MUST)。外側フレーム (Type=SUBSCRIBE 0x03,
/// Length=1, payload=[0x00]) は揃っているが、SUBSCRIBE のボディとしては Track Namespace が
/// 欠落している。ペイロード長は確定済みで追加データでは回復し得ないため、データ不足
/// (Ok(None)) ではなく MalformedMessage を返すこと。
#[test]
fn test_complete_frame_with_malformed_body_is_malformed_message() {
    let mut decoder = decoder();
    decoder.push(&[0x03, 0x01, 0x00]);

    let result = decoder.try_decode_message();
    assert!(
        matches!(result, Err(MessageError::MalformedMessage(_))),
        "完全な外側フレームの本文長不一致は MalformedMessage になること: {result:?}"
    );
}

/// 宣言長が受信上限を超える場合は、本文を待たずに FIELD_LENGTH_EXCEEDED になる
///
/// draft-ietf-moq-transport-23 §9 (Control Messages): 自側の MAX_CONTROL_MESSAGE_SIZE を
/// 超える Length の宣言は FIELD_LENGTH_EXCEEDED で閉じる (MUST)。巨大な本文を待たず、
/// 宣言長に応じた領域も確保しないことを、ヘッダのみを push して検証する。
#[test]
fn test_declared_length_over_limit_is_rejected_without_body() {
    let limit = DEFAULT_MAX_CONTROL_MESSAGE_SIZE;
    let mut buf = Vec::new();
    varint::encode(0x2F00, &mut buf); // SETUP
    varint::encode(limit + 1, &mut buf); // 上限を 1 バイト超える宣言長

    let mut decoder = decoder();
    decoder.push(&buf);
    let result = decoder.try_decode_message();
    assert!(
        matches!(result, Err(MessageError::FieldLengthExceeded(_))),
        "上限超過の宣言長は本文なしで拒否されること: {result:?}"
    );
}

/// 宣言長が受信上限ちょうどの場合は本文の到着を待つ
///
/// 境界値 (上限と同じ長さ) は正当であるため、本文が揃うまでは Ok(None)、揃えば成功する。
#[test]
fn test_declared_length_at_limit_waits_for_body() {
    // SETUP の本文を上限ちょうどの長さにする
    let limit = DEFAULT_MAX_CONTROL_MESSAGE_SIZE;
    let encoded = setup_frame_with_body_len(limit);
    assert_eq!(
        encoded.len(),
        2 + 3 + limit as usize,
        "type(2) + length(3) + body"
    );

    let mut decoder = decoder();
    // ヘッダ + 本文の先頭だけを入れる
    decoder.push(&encoded[..encoded.len() - 1]);
    assert!(
        decoder
            .try_decode_message()
            .expect("上限ちょうどの宣言はエラーにならない")
            .is_none(),
        "本文が揃うまでは Ok(None) であること"
    );
    decoder.push(&encoded[encoded.len() - 1..]);
    assert!(
        decoder
            .try_decode_message()
            .expect("上限ちょうどの本文はデコードできる")
            .is_some(),
        "本文が揃えばデコードできること"
    );
}

/// 既定値より大きい受信上限を設定したデコーダーは、その上限まで本文を受理する
///
/// 受信上限が実際に判定へ使われることを、既定上限 (65535) では拒否される 70,000 バイトの
/// 本文が、上限 1,000,000 のデコーダーでは受理されることで固定する。
/// 大きな受信上限を許す場合は transport のフロー制御窓もメッセージ全体以上にする必要がある
/// (draft-ietf-moq-transport-23 §15.5 (Resource Exhaustion))。
#[test]
fn test_non_default_receive_limit_accepts_larger_body() {
    let encoded = setup_frame_with_body_len(70_000);

    let mut large = MessageDecoder::new(1_000_000);
    large.push(&encoded);
    let result = large
        .try_decode_message()
        .expect("上限内の本文はデコードできること");
    assert!(
        matches!(result, Some(ControlMessage::Setup(_))),
        "既定値より大きい上限では 70,000 バイトの本文を受理すること: {result:?}"
    );

    let mut default_limit = decoder();
    default_limit.push(&encoded);
    let result = default_limit.try_decode_message();
    assert!(
        matches!(result, Err(MessageError::FieldLengthExceeded(_))),
        "既定上限では同じ宣言長を拒否すること: {result:?}"
    );
}

/// 非最小符号化された Length も受理する
///
/// draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers): decode 側は非最小
/// エンコーディングをエラーにしない。SETUP の本文長 0 を 3 バイトの vi64 で表しても
/// デコードできること。
#[test]
fn test_non_minimal_length_encoding_is_accepted() {
    let mut buf = Vec::new();
    varint::encode(0x2F00, &mut buf);
    // 0 を 3 バイトで表す (110xxxxx xxxxxxxx xxxxxxxx)
    buf.extend_from_slice(&[0xC0, 0x00, 0x00]);

    let mut decoder = decoder();
    decoder.push(&buf);
    let result = decoder
        .try_decode_message()
        .expect("非最小符号化の Length は受理されること");
    assert!(
        matches!(result, Some(ControlMessage::Setup(_))),
        "非最小符号化でも SETUP としてデコードできること: {result:?}"
    );
}

/// Length の varint が途中で切れた場合は Ok(None) を返す
///
/// 2 バイト vi64 の Length の 2 バイト目が未着の間は、データ不足として追加データを待つ。
#[test]
fn test_length_varint_split_across_pushes() {
    // Length を 2 バイトの vi64 で表した SETUP (本文 0)
    let mut buf = Vec::new();
    varint::encode(0x2F00, &mut buf);
    buf.extend_from_slice(&[0x80, 0x00]);

    let mut decoder = decoder();
    // Length の 1 バイト目までしか入れない
    decoder.push(&buf[..buf.len() - 1]);
    assert!(
        decoder
            .try_decode_message()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_none(),
        "Length の varint が途中なら Ok(None) であること"
    );
    decoder.push(&buf[buf.len() - 1..]);
    assert!(
        decoder
            .try_decode_message()
            .expect("テストフィクスチャの前提条件を満たす")
            .is_some(),
        "Length が揃えばデコードできること"
    );
}

/// ヘッダと本文を分割して push しても同じ結果になる
///
/// Length が確定したあと、本文を 1 バイトずつ追加しても最終的にデコードできること。
#[test]
fn test_header_and_body_split_across_pushes() {
    // SETUP の本文は空なので、本文を持つ GOAWAY でヘッダと本文の分割を検証する
    let message = ControlMessage::Goaway(shiguredo_moqt::message::Goaway {
        new_session_uri: b"moqt://example.com".to_vec(),
        timeout: 1000,
    });
    let encoded = message.encode(DEFAULT_MAX_CONTROL_MESSAGE_SIZE);
    let encoded = encoded.expect("テストフィクスチャの前提条件を満たす");

    // ヘッダ部分 (Type + Length) の長さを求める
    let (_, type_len) = varint::decode(&encoded).expect("Type の varint を読める");
    let (payload_len, length_len) =
        varint::decode(&encoded[type_len..]).expect("Length の varint を読める");
    let header_len = type_len + length_len;
    assert_eq!(
        header_len + payload_len as usize,
        encoded.len(),
        "Length は本文長と一致すること"
    );

    let mut decoder = decoder();
    decoder.push(&encoded[..header_len]);
    assert!(
        decoder
            .try_decode_message()
            .expect("ヘッダのみでは Ok(None) であること")
            .is_none(),
        "本文が未着なら Ok(None) であること"
    );
    for &byte in &encoded[header_len..] {
        decoder.push(&[byte]);
        let result = decoder
            .try_decode_message()
            .expect("テストフィクスチャの前提条件を満たす");
        if let Some(msg) = result {
            assert_eq!(msg, message, "分割受信でも同じメッセージになること");
            return;
        }
    }
    panic!("本文を全て push してもデコードされなかった");
}

/// 0 長の Length を 9 バイトの vi64 で表してもヘッダとして読める
///
/// vi64 の最大長 (9 バイト) をヘッダに使っても、Type と Length の境界が正しく決まること。
#[test]
fn test_nine_byte_length_encoding() {
    let mut buf = Vec::new();
    varint::encode(0x2F00, &mut buf);
    // 0 を 9 バイトで表す (11111111 + 8 バイトの 0)
    buf.push(0xFF);
    buf.extend_from_slice(&[0u8; 8]);

    let mut decoder = decoder();
    decoder.push(&buf);
    let result = decoder
        .try_decode_message()
        .expect("9 バイトの Length は受理されること");
    assert!(
        matches!(result, Some(ControlMessage::Setup(_))),
        "9 バイトの Length でも SETUP としてデコードできること: {result:?}"
    );
}

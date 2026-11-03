//! `MessageDecoder` の単体テスト
//!
//! バッファ付きインクリメンタルデコーダーの境界条件・エラーパスを検証する。
//! Length は vi64 (1〜9 バイト) であり、非最小符号化も受理する
//! (draft-ietf-moq-transport-23 §8.1 (Variable-Length Integers))。
use shiguredo_moqt::decoder::MessageDecoder;
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::parameter::DEFAULT_MAX_CONTROL_MESSAGE_SIZE;
use shiguredo_moqt::{message::Setup, parameter::SetupOptions, varint};

/// 制御メッセージの受信上限を既定値にしたデコーダーを作る
fn decoder() -> MessageDecoder {
    MessageDecoder::new(DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
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
    // SETUP の空の Setup Options は本文長 0 なので、上限ちょうどの本文を作るには
    // Unsupported メッセージで任意長の本文を宣言する
    let limit = DEFAULT_MAX_CONTROL_MESSAGE_SIZE;
    let body = vec![0u8; limit as usize];
    let message = ControlMessage::Unsupported {
        type_id: 0x06,
        request_id: None,
        body,
    };
    let encoded = message
        .encode(limit)
        .expect("上限ちょうどの本文は encode できる");
    assert_eq!(encoded.len(), 1 + 3 + limit as usize); // type(1) + length(3) + body

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

use shiguredo_moqt::decoder::MessageDecoder;
/// MessageDecoder の単体テスト
///
/// src/decoder.rs のインラインテストを公開 API に対して移動したもの。
/// バッファ付きインクリメンタルデコーダーの境界条件・エラーパスを検証する。
use shiguredo_moqt::error::MessageError;
use shiguredo_moqt::message::ControlMessage;
use shiguredo_moqt::{message::Setup, parameter::SetupOptions};

#[test]
fn test_decode_complete_message() {
    let setup = ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    });
    let encoded = setup
        .encode()
        .expect("正当なテスト入力の encode は成功する");

    let mut decoder = MessageDecoder::new();
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
    let setup = ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    });
    let encoded = setup
        .encode()
        .expect("正当なテスト入力の encode は成功する");

    let mut decoder = MessageDecoder::new();
    // 途中までしかデータを入れない
    decoder.push(&encoded[..encoded.len() / 2]);

    let result = decoder
        .try_decode_message()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result.is_none());
}

#[test]
fn test_decode_incremental() {
    let setup = ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    });
    let encoded = setup
        .encode()
        .expect("正当なテスト入力の encode は成功する");

    let mut decoder = MessageDecoder::new();

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
    let mut decoder = MessageDecoder::new();
    let result = decoder
        .try_decode_message()
        .expect("テストフィクスチャの前提条件を満たす");
    assert!(result.is_none());
}

#[test]
fn test_decode_varint() {
    let mut buf = Vec::new();
    shiguredo_moqt::varint::encode(0x2F00, &mut buf);

    let mut decoder = MessageDecoder::new();
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
    shiguredo_moqt::varint::encode(0x2F00, &mut buf);

    let mut decoder = MessageDecoder::new();
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
        .encode()
        .expect("正当なテスト入力の encode は成功する");
    encoded.extend(
        setup2
            .encode()
            .expect("テストフィクスチャの前提条件を満たす"),
    );

    let mut decoder = MessageDecoder::new();
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

#[test]
fn test_complete_frame_with_malformed_body_is_error() {
    // 外側フレーム (Type=SUBSCRIBE 0x03, Length=1, payload=[0x00]) は完全だが、
    // SUBSCRIBE のボディとしては Track Namespace が欠落している。
    // ペイロード長は確定済みで追加データでは回復し得ないため、
    // データ不足 (Ok(None)) ではなくエラーを返すこと。
    let mut decoder = MessageDecoder::new();
    decoder.push(&[0x03, 0x00, 0x01, 0x00]);

    let result = decoder.try_decode_message();
    assert!(
        matches!(result, Err(MessageError::UnexpectedEof)),
        "完全な外側フレームのボディ不正はエラーとして返されること"
    );
}

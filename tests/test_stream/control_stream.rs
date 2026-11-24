use shiguredo_moqt::message::{ControlMessage, Setup};
use shiguredo_moqt::parameter::{DEFAULT_MAX_CONTROL_MESSAGE_SIZE, SetupOptions};
use shiguredo_moqt::stream::{
    SETUP_STREAM_TYPE, classify_data_stream_type, encode_control_stream_setup,
};

/// SETUP の Type が stream type を兼ね、ワイヤには 1 回だけ現れる
///
/// draft-ietf-moq-transport-23 §6.4.1 (Unidirectional Streams) Table 2 / §9.1 (SETUP)
/// Figure 7: 制御ストリームの先頭 varint は SETUP メッセージの Type `0x2F00` であり、
/// これが stream type を兼ねる。stream type を前置すると `0x2F00` が 2 回現れ、
/// 1 回だけ送る実装と相互運用できない。
#[test]
fn control_stream_setup_starts_with_a_single_setup_type() {
    let setup = ControlMessage::Setup(Setup {
        options: SetupOptions::new(),
    });
    let encoded = encode_control_stream_setup(&setup, DEFAULT_MAX_CONTROL_MESSAGE_SIZE)
        .expect("SETUP をエンコードできること");

    // 0x2F00 の 2 バイト varint の直後に、本文長 0 の varint が続く (本文は空)
    assert_eq!(
        encoded,
        vec![0xAF, 0x00, 0x00],
        "SETUP の Type はワイヤに 1 回だけ現れること"
    );

    // 同じバイト列が SETUP 1 通としてデコードできる
    let (decoded, consumed) = ControlMessage::decode(&encoded).expect("SETUP をデコードできること");
    assert_eq!(
        consumed,
        encoded.len(),
        "エンコードした全バイトが SETUP 1 通として消費されること"
    );
    assert!(
        matches!(decoded, ControlMessage::Setup(_)),
        "先頭の 0x2F00 が SETUP の Type として読まれること"
    );
}

/// SETUP の Type (0x2F00) は data stream の種別として扱わない
///
/// 制御ストリームの先頭 varint は SETUP の Type が stream type を兼ねるため、
/// data stream の種別判定から除外する (draft-ietf-moq-transport-23 §6.4.1 Table 2)。
#[test]
fn setup_type_is_not_a_data_stream_type() {
    assert_eq!(
        classify_data_stream_type(SETUP_STREAM_TYPE),
        None,
        "SETUP の Type を data stream として扱わないこと"
    );
}

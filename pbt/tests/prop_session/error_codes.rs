//! Session のセッション終了コード分類のプロパティ
//!
//! draft-ietf-moq-transport-23 §11.1.1 (Object Status) / §11.3.1 (Subgroup Header) /
//! §12.2 (Session Termination Codes) を根拠とする。
//!
//! 任意の入力に対して、原因に応じた具体的なコードだけが返り、汎用の
//! `PROTOCOL_VIOLATION` に畳まれないことを検証する。

use pbt::common::test_runner;
use shiguredo_moqt::error::{MessageError, SESSION_INVALID_OBJECT_STATUS};
use shiguredo_moqt::session::types::{SessionState, TrackDataAcceptance};
use shiguredo_moqt::stream::datagram::ObjectDatagram;
use shiguredo_moqt::stream::subgroup::SubgroupHeader;
use shiguredo_moqt::varint;

use super::common::establish_pair;

/// draft-ietf-moq-transport-23 §11.1.1 で定義された Object Status
///
/// 0x0 Normal / 0x3 End of Group / 0x4 End of Track のみが定義値である。
const DEFINED_OBJECT_STATUS: &[u64] = &[0x00, 0x03, 0x04];

/// SUBGROUP_HEADER の Type Flags として定義された値かを返す
///
/// draft-ietf-moq-transport-23 §11.3.1 (Subgroup Header):
/// - bit4 (0x10) が必ず立っている
/// - 既知 bit (0x01 / 0x06 / 0x08 / 0x10 / 0x20 / 0x40) 以外が立っていない
/// - SUBGROUP_ID_MODE (bits 1-2) が 0b11 でない
fn subgroup_header_flags_are_valid(type_id: u64) -> bool {
    const KNOWN_MASK: u64 = 0x01 | 0x06 | 0x08 | 0x10 | 0x20 | 0x40;
    type_id & 0x10 != 0 && type_id & !KNOWN_MASK == 0 && (type_id & 0x06) >> 1 != 0b11
}

/// 任意の Status について、定義値だけが受理され、それ以外は INVALID_OBJECT_STATUS になること
///
/// `recv_object_datagram` は Track Alias の解決より前に Status を検証するため、
/// 未知の Track Alias でも検証結果を観測できる。
#[test]
fn object_datagram_status_classification_is_total() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(128, |ctx| {
        let status = noprop::sample_u64(ctx);
        let (mut client, _server) = establish_pair();
        let result = client.recv_object_datagram(&ObjectDatagram {
            track_alias: 99,
            group_id: 0,
            object_id: 1,
            publisher_priority: Some(128),
            properties_data: None,
            end_of_group: false,
            status: Some(status),
        });
        match result {
            Err(err) => {
                assert!(
                    !DEFINED_OBJECT_STATUS.contains(&status),
                    "定義済み status {status:#x} は拒否されないこと"
                );
                assert_eq!(
                    err.code, SESSION_INVALID_OBJECT_STATUS,
                    "不正 status {status:#x} は INVALID_OBJECT_STATUS (0x202) であること"
                );
                assert_eq!(
                    client.state(),
                    SessionState::Closing,
                    "不正 status はセッションを閉じること"
                );
            }
            Ok(acceptance) => {
                assert!(
                    DEFINED_OBJECT_STATUS.contains(&status),
                    "不正 status {status:#x} は受理されないこと"
                );
                // 購読を作っていないため Track Alias は未知として扱われる
                assert_eq!(
                    acceptance,
                    TrackDataAcceptance::UnknownTrackAlias,
                    "未知の Track Alias は破棄せず UnknownTrackAlias を返すこと"
                );
            }
        }
        Ok(())
    })?;
    Ok(())
}

/// 任意の Type Flags について、定義外の値だけが INVALID_STREAM_HEADER になること
///
/// Type だけを与え、後続フィールドを欠落させるため、フラグが正当なら `UnexpectedEof`、
/// 不正なら `InvalidStreamHeader` になる。
#[test]
fn subgroup_header_flags_classification_is_total() -> noprop::TestResult {
    let mut runner = test_runner()?;
    runner.run(256, |ctx| {
        let type_id = noprop::sample_u64(ctx);
        let mut bytes = Vec::new();
        varint::encode(type_id, &mut bytes);
        match SubgroupHeader::decode(&bytes) {
            Err(MessageError::InvalidStreamHeader(_)) => {
                assert!(
                    !subgroup_header_flags_are_valid(type_id),
                    "フラグ {type_id:#x} は不正として拒否されること"
                );
            }
            Err(MessageError::UnexpectedEof) => {
                assert!(
                    subgroup_header_flags_are_valid(type_id),
                    "フラグ {type_id:#x} は正当として受理されること"
                );
            }
            other => panic!("フラグ {type_id:#x} の decode 結果が想定外である: {other:?}"),
        }
        Ok(())
    })?;
    Ok(())
}

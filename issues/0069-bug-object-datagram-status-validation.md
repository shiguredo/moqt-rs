# send_object_datagram の status 値域を検証する

- Created: 2026-09-13
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-object-datagram-status-validation
- Polished: {YYYY-MM-DD}

## 目的

`Session::send_object_datagram` が未知の Object Status を受理しないようにし、拒否時に subscription の状態を進めないようにする。Session は protocol state machine であり、公開 API から不正な protocol 値が渡された場合は、呼び出し側が wire バイト列を組み立てる前の段階で拒否する必要がある。

## 現状

- `src/session/data.rs` の `Session::send_object_datagram` は `properties_data` を `validate_datagram_properties_blob` で検証するが、`status` の値域は検証しない。
- `src/stream/datagram.rs` の `ObjectDatagram::encode` は `validate_object_status` により 0x0 (Normal) / 0x3 (End of Group) / 0x4 (End of Track) 以外を `ProtocolViolation` として拒否する。
- draft-ietf-moq-transport-21 §11.1.2 (Object Status) は次のように規定する。

  > Any other value SHOULD be treated as a protocol error and the session SHOULD be closed with a PROTOCOL_VIOLATION (Section 12.2).

- 実際に `send_object_datagram` へ `status = Some(0x5)` を渡すと `Ok(())` が返り、`largest_received_location` がその位置に更新される。続けて `ObjectDatagram::encode` で同じ status をエンコードすると `ProtocolViolation("unknown object status value")` になる。送信されていない Object の位置だけが Session に公開済みとして記録される。
- `ObjectDatagram::encode` を経由せずに自前で wire バイト列を組み立てる呼び出し側では、未知 status がそのまま送信されうる。
- `tests/test_session/data_stream.rs` の `send_object_datagram_rejected_for_properties_with_non_normal_status` は、拒否時に `largest_received_location` が `None` のままであることを固定しており、拒否時に状態を汚染しないのが本 API の契約である。

## 設計方針

- `send_object_datagram` の入力検証で `status` の値域を検証し、0x0 / 0x3 / 0x4 以外は `SESSION_PROTOCOL_VIOLATION` で拒否する。
- 検証は既存の `properties_data` 検証と同じ段階 (フィルタ評価と `largest_received_location` 更新より前) で行う。
- 既知 status と `None` の既存挙動は変えない。

## 完了条件

- `send_object_datagram` に 0x0 / 0x3 / 0x4 以外の status を渡すと `SESSION_PROTOCOL_VIOLATION` で拒否されること
- 拒否時に `largest_received_location` が更新されないこと
- 既知 status (0x0 / 0x3 / 0x4) と `None` の送信および最大位置更新の既存挙動が維持されること
- 上記を検証するテストが追加されていること

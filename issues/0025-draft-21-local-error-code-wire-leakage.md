# ローカル専用エラーコードの wire 流出を API で防ぐ

- Created: 2026-09-10
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-local-error-code-wire-leakage

## 目的

`SESSION_LOCAL_FILTER_MISMATCH` と `SESSION_LOCAL_DATAGRAM_TIMEOUT` が wire へ流出する footgun をなくす。これらは実装内部の値であり、draft-ietf-moq-transport-21 のどのエラーコードレジストリにも存在しない。

## 現状

`src/error.rs` の doc は両値について「wire に送出してはならない」と明記しているが、返り値型は wire エラーと同じ `SessionError`。`Session::close(code, reason)` / `Session::fail` は任意の `u64` を受けるため、Application が `err.code` をそのまま渡すと未定義コードが QUIC CONNECTION_CLOSE や WebTransport close に載る。

返却箇所: `src/session/data.rs` の `send_subgroup_object` / `send_object_datagram` / datagram timeout 経路。

## 設計方針

ローカル拒否を `SessionError` ではなく別型 (例: `SendRequestError` や専用 enum variant) で返す。少なくとも `Session::close` / `Session::fail` 側で当該値域を弾き、`SessionError` にローカル値を載せない。公開 API の変更になるため `CHANGES.md` に明記する。

## 完了条件

- ローカル専用コードが `SessionError` として返らないこと
- `close` / `fail` にローカル専用値を渡しても wire へ出ないこと
- ローカル拒否のテストが追加されていること

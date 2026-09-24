# example と library の MSF fragment 検証の役割分担を明記する

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-fragment-validation-gap
- Polished: {YYYY-MM-DD}

## 目的

example の `parse_url` と library の `msf::uri::parse_msf_uri` が同じ `#msf:` fragment を別々に扱うことを文書に明記し、
利用者がどちらの検証を通るのかを判断できるようにする。

## 現状

`examples/moqt-transport/src/lib.rs` の `parse_url` は draft-ietf-moq-transport-21 §6.1.1 の構文
(`<type>:<value>` と type の文字種) だけを検証し、type 固有の値は検証しない。
`#msf:` (空の value) は `MoqtFragment { fragment_type: "msf", value: "" }` として受理される。

library の `src/msf/uri.rs` の `parse_msf_uri` は MSF §11.1 の
`msf-fragment = "msf:" msf-fragment-value` / `msf-fragment-value = track-identifier [ "&" parameter-list ]` /
`track-identifier = 1*( pchar-no-amp / "/" )` に従い、`moqt://example.com/path#msf:` を
`InvalidCatalog("MSF fragment is empty")` として拒否する。

example は fragment の値を使わないため実害は無いが、
同じ `#msf:` が example では受理され library では拒否される差は文書に書かれていない。

## 設計方針

- example の役割は §6.1.1 の構文検証までとし、type 固有の値検証は library が担うことを `ServerUrl::fragment` と
  `MoqtFragment` の doc、`examples/README.md` に明記する
- `msf` fragment の例 (`#msf:track` / `#msf:`) を挙げ、値の解釈と検証は `parse_msf_uri` が行うことを書く
- example の `parse_url` の検証範囲は変えない (type 固有の検証を example に足さない)
- type の登録有無を検証しない現状の方針 (§16.3 の registry) と矛盾しない書き方にする

## 完了条件

- `MoqtFragment` / `ServerUrl::fragment` の doc に、example は構文のみを検証し値は解釈しないことが書かれていること
- `examples/README.md` の fragment の説明に、`msf` の値の検証は library が行うことが書かれていること
- コードの挙動 (テストを含む) が変わらないこと

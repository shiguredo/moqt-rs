# example と library の MSF fragment 検証の役割分担を明記する

- Created: 2026-09-24
- Completed: 2026-09-28
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

## 解決方法

本 issue は前提が崩れ、報告した問題が現行実装に存在しないため closed とする (判定: 前提崩壊・方針変更 / 実装済み)。

- 前提の崩壊: 「現状」は example の `parse_url` が `#msf:` (空の value) を受理すると記すが、C4M 対応で example が MSF fragment を使うようになった
  (`CHANGES.md` の "[ADD] example が URL の MSF fragment (`#msf:<track-identifier>&c4m=<token>`) を MSF 仕様に従って検証し、`c4m` パラメータを SETUP の AUTHORIZATION_TOKEN (Token Type CAT) として送信する")。
  現行の `examples/tokio-moq/src/lib.rs` の `parse_url` は `msf` 型の value を
  `shiguredo_moqt::msf::uri::parse_msf_fragment` で検証するため、`#msf:` は
  `invalid MSF fragment` として拒否される
- 実測: `cargo test -p tokio-moq --lib` の `parse_url_rejects_invalid_msf_fragment`
  (value 空のケースを含む) と `parse_url_accepts_empty_fragment_value` が通り、
  `msf` 型の空 value が拒否・`msf` 以外の型の空 value は受理されることを確認した
- 役割分担の明記は既に実現している: `examples/tokio-moq/src/lib.rs` の
  `MoqtFragment` / `ServerUrl::fragment` / `parse_url` の doc と、`examples/README.md` の
  「URL スキーム」「C4M 認可トークン」に、example が `msf` 型だけ値検証と `c4m` の解釈を
  行い、他の型は構文 (§6.1.1) のみを検証することが書かれている
- 本 issue の完了条件 1 は「example は構文のみを検証し値は解釈しない」の文書化を求めるが、
  現行実装 (msf 型は example が `parse_msf_fragment` で検証) に当てはめると誤った記述になる。
  完了条件 3 の「コードの挙動が変わらないこと」も、C4M 対応により既に満たされない
- 対象パスの陳腐化: `examples/moqt-transport/src/lib.rs` は rename 済みで
  `examples/tokio-moq/src/lib.rs` が現行パスである
- 重複なし: closed 0133 (fragment の経路混入の修正) や closed 0128 (MSF URI の percent-encoding) と
  対象範囲が重ならないことを確認した

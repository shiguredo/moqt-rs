# split_authority_path が `#` を path の終端として扱わない

- Created: 2026-09-24
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-split-authority-path-hash
- Polished: {YYYY-MM-DD}

## 目的

`examples/moqt-transport/src/lib.rs` の `split_authority_path` を単体で呼んでも fragment が path に混入しないようにする。
現在は `parse_url` が先に fragment を分離する前提で成立しており、前提を忘れて呼ぶと RFC 3986 に反する path を返す。

## 現状

`split_authority_path` は `pub` で、authority を最初の `/` または `?` までとして残りを path にする。

```rust
let end = rest.find(['/', '?']).unwrap_or(rest.len());
```

RFC 3986 §3.2 は authority が `/` / `?` / `#` で終端すると定めるため、`split_authority_path("example.com/path#type:value")` は
authority `example.com` / path `/path#type:value` を返す。
`parse_url` は `split_fragment` の後に呼ぶため現状の接続経路に実害は無いが、この関数だけを見ると fragment を分離しない。
リポジトリ内の呼び出しは `parse_url` とテストだけで、前提は doc コメントに書いてある。

## 設計方針

- `#` も authority の終端として扱い、path に `#` 以降を含めない
- `split_authority_path` の戻り値の形 (`(String, String)`) は変えず、`parse_url` は fragment を先に分離する現状の順序を維持する
- 既存テスト (authority と path の分離、query を path に含める、query のみで `/` を補う) は維持する
- 単体で `#` を渡したときの期待値をテストで固定する

## 完了条件

- `split_authority_path("example.com/path#type:value")` の path が `/path` になり、`#` 以降を含まないこと
- `split_authority_path("example.com#type:value")` の authority が `example.com` になり、path が `/` になること
- `parse_url` の既存の挙動 (fragment の分離、authority が空のときのエラー、query の扱い) が変わらないこと
- RFC 3986 §3.2 の引用と、単体で呼んだときの期待値をコメントに残すこと

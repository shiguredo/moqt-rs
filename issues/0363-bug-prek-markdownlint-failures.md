# prek の markdownlint が issues 配下の Markdown で失敗し続ける

- Created: 2026-10-21
- Completed: {YYYY-MM-DD}
- Branch: develop
- Polished: {YYYY-MM-DD}

## 目的

develop の CI の prek ジョブが markdownlint-cli2 で失敗し続けており、CI が赤い状態が
常態化している。pre-commit / pre-push と同じ静的検査を CI で通る状態に戻す。

## 現状

- develop の CI の `prek` ジョブが失敗している。直前の commit (`49c711e`) の CI でも
  `failure` であり、今回の変更で発生したものではない
- 失敗しているフックは `markdownlint-cli2` で、違反は 40 件 (MD013 line-length 39 件、
  MD032 blanks-around-lists 1 件)。すべて `issues/` 配下の Markdown である
- MD013 は `.markdownlint.jsonc` の `line_length` 300 を超える行で、長い説明文が 1 行に
  詰め込まれているものが対象である
- MD032 は `issues/closed/0310-draft-22-fetch-stream-validation.md` の blockquote 内の
  リストの直前に空行が無いものである
- `markdownlint-cli2` 以外の prek フック (cargo fmt / clippy / test、tombi、builtin) は
  通っている

## 変更対象

- `issues/0302-draft-22-sender-delivery-timeout.md`
- `issues/0305-draft-22-response-parameter-scope.md`
- `issues/0306-draft-22-send-response-prevalidation.md`
- `issues/0308-draft-22-stop-sending-reset-open-streams.md`
- `issues/0311-draft-22-error-classification-consistency.md`
- `issues/0314-change-consume-payload-contract.md`
- `issues/0331-bug-publisher-namespace-validation.md`
- `issues/0348-bug-playout-target-latency-ceiling.md`
- `issues/closed/0310-draft-22-fetch-stream-validation.md`

## 設計方針

- `.markdownlint.jsonc` のルールは変更しない (MD013 の 300 は維持する)
- 文章・コード・リンクの内容は変えず、行の折り返しと空行の追加だけで違反を解消する
- 折り返しは既存の本文と同じ約 90〜110 文字に合わせ、インラインコードとリンクの内部では
  折らない
- リスト項目の継続行は本文と同じく 2 スペースでインデントする
- 折り返しで行頭が句読点だけにならないようにする

## 完了条件

- `prek run markdownlint-cli2 --all-files` が成功すること
- `prek run --all-files` が成功すること
- 折り返しと空行の追加以外に Markdown の内容が変わっていないこと
- CI の prek ジョブが成功すること

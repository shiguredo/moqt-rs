# E2E テストの namespace が並列実行で衝突するのを直す

- Created: 2026-10-14
- Completed: {YYYY-MM-DD}
- Branch: feature/fix-e2e-namespace-collision
- Polished: {YYYY-MM-DD}

## 目的

E2E Test の `publish_and_subscribe_audio_datagram_roundtrip` が
`request 2 rejected: code=32 reason=already_owned` で失敗する。`code=32` は
「その track は既に他のセッションが publish している」ことを表す。

## 現状

- `e2e_namespace()` はプロセス ID と `SystemTime::now()` のナノ秒から名前を作る
- テストは同じプロセスで並列に走る。`SystemTime::now()` の分解能は OS 依存であり、
  macOS ではマイクロ秒であるため、2 つのテストが同じ名前を作りうる
- 2 つのテストはどちらも catalog track を PUBLISH する。名前が同じになると、後から
  publish した側が `already_owned` で拒否され、publisher が終了する
- `WorkDir` でも同じ問題があり、テスト名を名前へ入れて直した

## 設計方針

- `e2e_namespace(test)` にして、テストごとの名前を混ぜる
- 呼び出し側は `e2e_namespace("roundtrip")` と `e2e_namespace("audio-datagram")` にする
- 別のマシンで同時に走る実行と衝突しないよう、プロセス ID と時刻は残す
- 同じプロセスで 2 つの名前を作っても衝突しないことをテストで固定する

## 完了条件

- 同じプロセスで 2 つのテストが並列に走っても、namespace が衝突しないことがテストで
  固定されていること
- E2E Test が成功すること
- `make test` / `make clippy` / `make fmt` が通ること
- ソースコードに issue 番号や issue への言及を書かないこと

## 解決方法

(対応時に記入)

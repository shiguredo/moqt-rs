# 変更履歴

- UPDATE
  - 後方互換がある変更
- ADD
  - 後方互換がある追加
- CHANGE
  - 後方互換のない変更
- FIX
  - バグ修正

## develop

- [FIX] SUBSCRIBE_TRACKS の bidi stream 終端後の PUBLISH の bidi stream 終端でセッションを閉じないようにし、RequestTerminated の kind を実際の要求種別に合わせる
  - @voluntas
- [FIX] MSF の isLive=false で targetLatency / buffers をエンコードしないようにする
  - @voluntas

### misc

- [UPDATE] MSF の track / cloneTrack の JSON メンバー書き出しと共通検証を共通化する
  - @voluntas

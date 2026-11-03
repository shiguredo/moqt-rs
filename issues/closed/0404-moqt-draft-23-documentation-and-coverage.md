# MOQT draft 23 の仕様参照と実装状況を更新する

- Created: 2026-11-03
- Completed: 2026-11-04
- Branch: develop
- Polished: 2026-11-03

## 目的

draft 23 への実装追従後、利用者向け説明とコードの根拠節を新しい仕様に揃える。
根拠は [draft-ietf-moq-transport-23](../refs/moq/draft-ietf-moq-transport-23.txt) の全文差分と Appendix A.1。

## 現状

README.md、docs/moqt.md、docs/missing-features.md と src/ 配下の多数の説明が draft 22 を参照する。
FETCH_OK の削除に伴い §9.13 以降が移動し、Parameters は §9.19、Error Codes は §16.12 になった。
末尾の番号だけを機械置換すると、旧規則を新仕様の規則として誤記する。

## 設計方針

- 各対応 issue の実装結果を確認してから、README、公開 SKILL、docs とコードコメントを更新する。
- 実装済み・未対応・限定 endpoint の対象外を分け、仕様更新だけで全対応と書かない。
- §15.6 の timeout の待機箇所とトレードオフを、既存のタイマーと docs/missing-features.md に照合する。新しい一律 timeout 値や MUST を作らない。
- モジュールの移動だけの説明や図表・日付・参照文献の更新は、新しい実装機能として扱わない。

## draft 22 → 23 の差分対照

| 変更 | 追跡先・判定 |
| --- | --- |
| draft 番号に対応する ALPN / WT サブプロトコル | 0378 |
| varint Message Length と MAX_CONTROL_MESSAGE_SIZE | 0379 |
| FILL_PARAMETERS の個数符号化 | 0380 |
| FORWARD → PAUSED、ID と意味の反転 | 0381 |
| FETCH_OK を REQUEST_OK へ統合、LARGEST_OBJECT から応答終点を導出 | 0382 |
| 相対 Location Filter の単独 FETCH を INVALID_RANGE で拒否 | 0383 |
| Error Details と REDIRECT の KVP 化 | 0384 |
| UNSUPPORTED_PROPERTY Detail | 0385 |
| 具体的な session error code、auth token 解析エラー、優先分類 | 0386 |
| REQUEST_UPDATE の受信順処理と OK 順序、fill の非 coalescing | 0387 |
| PUBLISH の fill、初回 FETCH_HEADER の Request ID | 0388 |
| SUBSCRIBE_TRACKS の明示スコープ | 0389 |
| SUBSCRIBE_TRACKS_ID | 0390 |
| 全 OK の EXPIRES、TRACK_STATUS_OK の仮の購読期限 | 0391 |
| request receiver だけが GOAWAY を送る | 0392 |
| 空の Track Name の禁止、.session の旧例外の削除 | 0393 |
| DYNAMIC_GROUPS 判明前の NEW_GROUP_REQUEST | 0394 |
| Gone 状態・End of Gone Range | 0395 |
| FETCH End of Range の delta 化 | 0396 |
| 同 Group の FETCH Object ID Delta + 2 | 0397 |
| FETCH End of Group / Track、prior Object ID の sentinel | 0398 |
| Object の存在と不存在の矛盾を malformed とする | 0399 |
| 対応済み Property のスコープ違反 | 0400 |
| 既知 Property の多重可否、未知 Property の重複許可 | 0401 |
| TRACK_STATUS_OK の未知 Mandatory Track Property | 0402 |
| UNAVAILABLE、PUBLISH_DONE EXPIRED_AUTH_TOKEN、旧コードの削除 | 0403 |
| Request ID の重複検出義務の緩和 | 現行の検出を続けても適合。再利用禁止は維持する |
| Mandatory Track Property 範囲内の GREASE 除外 | TrackProperties::has_unknown_mandatory と Object Properties の decoder で対応済み |
| 後続 Object に Properties があり得る場合の SUBGROUP_HEADER ビット | SubgroupHeader が明示フラグを持ち、moq-pub の送信元が全 Object 用に設定済み |
| overlapping namespace subscription、prefix 更新の広告撤回・再 PUBLISH 抑制 | namespace 機能は未対応。NOT_SUPPORTED を継続する。検証するスコープは 0389 で追従 |
| 同一 prefix の複数 PUBLISH_NAMESPACE、NAMESPACE の Parameters / 再通知 | namespace 機能は未対応。生 body を保持する Unsupported の受信方針を継続する |
| upstream LARGEST_OBJECT の計算条件、request stream credit、namespace fan-out、Gone cache | リレーの責務であり、このリポジトリの実装対象外 |
| timeout の待機箇所・値の選定、参照節・図表・説明修正 | 本 issue の説明更新 |

## 完了条件

- 各追従 issue の結果と利用者向け仕様対応表が一致すること。
- 旧 wire 形式の説明を新仕様の引用として残さず、歴史的な参照は理由を付けて区別すること。
- 節番号・図表・パラメータ名・エラー名の参照を本文で確認すること。
- リレー機能や namespace 機能の追加を対象にしないこと。

## 依存・関連

- 先行: [0378](0378-moqt-draft-23-protocol-identifier.md)
- 先行: [0379](0379-moqt-draft-23-control-message-length-limit.md)
- 先行: [0380](0380-moqt-draft-23-fill-parameters-count.md)
- 先行: [0381](0381-moqt-draft-23-paused-parameter.md)
- 先行: [0382](0382-moqt-draft-23-fetch-ok-request-ok.md)
- 先行: [0383](0383-moqt-draft-23-fetch-relative-location-filter.md)
- 先行: [0384](0384-moqt-draft-23-request-error-details.md)
- 先行: [0385](0385-moqt-draft-23-unsupported-property-error-detail.md)
- 先行: [0386](0386-moqt-draft-23-session-error-classification.md)
- 先行: [0387](0387-moqt-draft-23-request-update-ordering.md)
- 先行: [0388](0388-moqt-draft-23-publish-fill-fetch.md)
- 先行: [0389](0389-moqt-draft-23-subscribe-tracks-parameter-scope.md)
- 先行: [0390](0390-moqt-draft-23-subscribe-tracks-id-parameter.md)
- 先行: [0391](0391-moqt-draft-23-expires-response-scope.md)
- 先行: [0392](0392-moqt-draft-23-request-stream-goaway-role.md)
- 先行: [0393](0393-moqt-draft-23-nonempty-track-name.md)
- 先行: [0394](0394-moqt-draft-23-new-group-before-track-properties.md)
- 先行: [0395](0395-moqt-draft-23-fetch-gone-range.md)
- 先行: [0396](0396-moqt-draft-23-fetch-range-delta.md)
- 先行: [0397](0397-moqt-draft-23-fetch-object-id-delta.md)
- 先行: [0398](0398-moqt-draft-23-fetch-group-track-end.md)
- 先行: [0399](0399-moqt-draft-23-nonexistence-conflict.md)
- 先行: [0400](0400-moqt-draft-23-property-scope.md)
- 先行: [0401](0401-moqt-draft-23-property-duplicates.md)
- 先行: [0402](0402-moqt-draft-23-track-status-mandatory-properties.md)
- 先行: [0403](0403-moqt-draft-23-request-and-reset-error-registry.md)

## 解決方法

draft-23 の全文差分と Appendix A.1 を根拠に、両 draft の目次と本文の対応表を作ってから置換した。
末尾番号の機械置換は行っていない。

- 節番号の移動は次のとおり。コードコメント・テスト・`pbt/`・`fuzz/`・`e2e-tests/`・`crates/`・
  `docs/`・`README.md`・`skills/shiguredo-moqt/SKILL.md` の 159 ファイルに反映した
  - `§9.13` → `§9.12` (TRACK_STATUS)、`§9.14` → `§9.13` (PUBLISH_NAMESPACE)、
    `§9.15` → `§9.14` (SUBSCRIBE_NAMESPACE)
  - `§9.20` → `§9.19`、`§9.20.1`〜`§9.20.20` → `§9.19.1`〜`§9.19.20`、
    `§9.20.21` (INCLUDE_PROPERTIES) → `§9.19.22` (SUBSCRIBE_TRACKS_ID の挿入で 1 つ後ろ)
  - `§11.4.1.2` (End of Range) → `§11.4.2`、`§16.11` (Error Codes) → `§16.12`、
    `§16.11.1`〜`§16.11.4` → `§16.12.1`〜`§16.12.4`
  - `§9.4.1` (Redirect Structure) → `§9.4.2` (Error Details)、
    `§9.4.2` (REQUEST_ERROR Message Format) → `§9.4.1`
  - `§7.4.1` / `§7.6.1` (Relay Switchover) → `§7.5.1` / `§7.7.1`
  - Table 20〜22 (エラーコード表) → Table 21〜23、Figure 3 / 6 → Figure 4 / 7、
    Appendix A.3 / A.5 → A.4 / A.6
- FETCH_OK の廃止に伴い、`§9.12 (FETCH_OK)` を引用していた 6 箇所を書き換えた。成功応答は
  `§9.3 (REQUEST_OK)`、FETCH_OK という名称は `§1.5 (Response Message Naming)` の別名、
  応答範囲の終端は `§3.2 (Fetch)` / `§9.19.17 (LARGEST OBJECT Parameter)` を引く
- 旧 wire 形式・旧規則を新仕様の引用として残していた箇所を直した。`FORWARD` と
  `Pass = Forward AND ...` は PAUSED と `Pass = (NOT Paused) AND ...` へ、draft-23 で削除された
  "Since Objects can be delivered out of order" は `§2.2 (Subgroups)` の stream 単位の順序保証へ
  置き換えた
- 引用文とエラー名を draft-23 の文面に合わせた。LARGEST_OBJECT の MUST、Request ID の重複検出、
  REQUEST_UPDATE の 2 ケース (`UNEXPECTED_MESSAGE`)、Parameter Scope (`INVALID_PARAMETER`)、
  Location Filter Type (`INVALID_PARAMETER`) と End Group 溢出 (`DELTA_ENCODING_OVERFLOW`)、
  Object Status (`INVALID_OBJECT_STATUS`)、切り詰められた Object (`TRUNCATED_OBJECT`)
- 利用者向けドキュメントを更新した
  - `README.md` / `skills/shiguredo-moqt/SKILL.md` / `crates/README.md` の対応仕様を
    `draft-23` にした
  - `docs/moqt.md` は Setup Options を 8 種にし、FETCH_OK が REQUEST_OK の別名であること、
    PAUSED、FILL_PARAMETERS の個数符号化、相対 Location Filter の拒否、REQUEST_UPDATE の
    受信順処理と FILL_PARAMETERS の非 coalescing、空 Track Name の拒否、`DYNAMIC_GROUPS`
    未受信の `NEW_GROUP_REQUEST` 受理、Malformed Track 条件 6〜10 を追記した
  - `docs/missing-features.md` は対象仕様を draft-23 にし、§15.6 (Timeouts) の待機箇所と
    既存タイマー (`CONTROL_MESSAGE_TIMEOUT` / `DATA_STREAM_TIMEOUT` / 購読の Delivery Timeout /
    `GOAWAY_TIMEOUT`) の対応とトレードオフを追記した。一律の timeout 値や新しい MUST は
    追加していない。`SUBSCRIBE_DONE` → `PUBLISH_DONE` の改名が draft-13 由来であることも確認した
- draft-22 への言及は、draft-22 の相手を誤受理しないことを確かめる ALPN のテストだけに残した。
  これは相手のプロトコル版を指す記述であり、現在の仕様の引用ではない
- MoQ リレー機能・namespace 機能の追加は行っていない。`SUBSCRIBE_TRACKS` /
  `PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` は引き続き未対応として記述した
- モジュール移動・図表・日付・参照文献の更新だけの項目は、新しい実装機能として扱っていない

## 完了条件の検証

| 完了条件 | 結果 | 検証方法 |
| --- | --- | --- |
| 各追従 issue の結果と利用者向け仕様対応表が一致すること | 満たす | 0378〜0403 の結果と docs を突き合わせ、PAUSED・FETCH_OK・EXPIRES・Gone・UNAVAILABLE などの記述を実装に合わせた。未対応は未対応と明記した |
| 旧 wire 形式の説明を新仕様の引用として残さないこと | 満たす | draft-22 の節参照と `§9.20` / `§7.6.1` / `§11.4.1.2` / `§16.11.x` の参照が 0 件であることを確認した。引用文は draft-23 の文面と機械照合した |
| 節番号・図表・パラメータ名・エラー名の参照を本文で確認すること | 満たす | 節番号が draft-23 に実在し、括弧内の節名が題名と一致することを機械的に確認した。エラー名は §16.12 と `src/error.rs` を突き合わせた |
| リレー機能や namespace 機能の追加を対象にしないこと | 満たす | 変更はコメント・ドキュメント・issue のみで、公開 API と状態機械は変えていない。`SUBSCRIBE_TRACKS` / `PUBLISH_NAMESPACE` / `SUBSCRIBE_NAMESPACE` は未対応のまま |

- `cargo fmt --all -- --check` 成功
- `cargo clippy --workspace --all-targets -- -D warnings` 成功
- `cargo test --workspace` 成功
- `RUSTDOCFLAGS="-D warnings" cargo doc -p shiguredo_moqt --no-deps` 成功
- 変更した Markdown への `prek run markdownlint-cli2 --files ...` 成功

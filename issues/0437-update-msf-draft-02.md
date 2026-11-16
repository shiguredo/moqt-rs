# MSF draft-02 に準拠する

- Created: 2026-11-16
- Completed: {YYYY-MM-DD}
- Branch: feature/update-msf-draft-02
- Polished: {YYYY-MM-DD}

## 目的

`refs/moq/draft-ietf-moq-msf-02.txt` へ参照仕様を更新したが、実装は draft-01 のままである。`src/msf.rs` の `MSF_VERSION` は `"draft-01"` で、`MsfCatalogDocument::decode` は異なる version を `MessageError::InvalidCatalogVersion` で拒否するため、v02 のカタログはすべて受理できない。

draft-02 はカタログの名前表現、packaging 値、delta update、Event Timeline の形式、LOC の Stream 割り当て、multi-variant 切り替えなど広範囲に規範要件を変更した。バージョンだけを先に更新すると、v02 を名乗りながら v02 の MUST を満たさないカタログを受理する期間が生じる。カタログ・タイムライン・伝送の変更は相互に依存するため、1 つの issue で順序を定めて対応する。

## 現状

- バージョン: `MSF_VERSION` は `"draft-01"`。`MsfCatalog::new` も draft-01 を出力する。
- カタログ: `packagingMetadata` / `accessibleWindowTime` / `keyframeSpacing` / `streamMapping` を表現できない。`MsfPackaging` は `"catalog"` を持たず、`depends` は `Vec<String>` で renderGroup (Number) を受理しない。
- カタログ: track-level `token` を出力し続け、`connectionUri` という key を使う。`cipherSuite` は `encryptionScheme` があれば常に必須としている。`initDataList` は `"inline"` だけを受理する。
- delta update: `MsfDeltaOperation` は add / remove / clone のみで `"update"` を拒否する。`generatedAt` は任意で、`isComplete` / `publishTracks` / `initDataList` を持つ delta を拒否しない。再追加時の属性変更を拒否する v01 の不変検証が残る。
- タイムライン: `MsfMediaTimeline` は media time 順を検証せず、template 属性と explicit payload の併用も検出しない。media timeline / event timeline に `mimeType == "application/json"` と `depends` 非空を要求する。Event Timeline の index key は小文字 `t` / `l` / `m` で、`data` は Object に限定される。`MSF_EVENT_TIMELINE_HEADER` (0x7B) に対応していない。
- 名前と URL: `MsfTrack.namespace` は生の string で、`/` 区切り・1〜32 fields の検証がない。`crates/moq-sub` はカタログ namespace を `-` 区切りとして解釈し、`crates/moq-pub` も `-` 区切りで出力する。fragment の予約パラメータは `c4m` のみ。変数値は `+` `/` `=` `.` を拒否する。
- 伝送: `crates/moq-pub` は `SubgroupIdMode::Zero` の group モード固定で、`streamMapping` を通知しない。`keyframeSpacing` と multi-variant の Group 割り当てがない。`crates/moq-sub` は alternate group を認識せず、切り替え手順もない。カタログと media の優先度は同じ 128 である。
- 参照: コード中の `draft-ietf-moq-msf-01` 参照は 86 ファイル・600 箇所超あり、節番号付きの約半数が v02 では別の節を指す。`docs/msf.md` / `docs/missing-features.md` / crate README も draft-01 を参照する。

## 設計方針

### 1. バージョンと受理

- `MSF_VERSION` を `"draft-02"` にし、draft-01 は受理しない。未知 version を拒否する挙動は維持する。
- バージョン更新は、カタログ受理系の変更が入った後に反映する。単独で先に入れない。

### 2. 名前と URL

- カタログの namespace を `/` 区切りとして 1〜32 fields に分解し、各 field を UTF-8 バイト列に変換する。空 field、先頭・末尾・連続する `/`、32 fields 超を拒否する。
- track name は空文字と NUL を拒否し、比較は mapped bytes の完全一致で行う。BOM と未対サロゲートの拒否をテストで固定する。
- `crates/moq-pub` と `crates/moq-sub` の namespace 解釈を `-` 区切りから `/` 区切りに揃える。
- 変数値に `+` `/` `=` `.` を加え、変数名の文字種制限を緩和する。fragment は最初の `=` で分割する。
- 予約 fragment パラメータを `cat` に改名し、`c4m` は予約として扱わない。公開 API の改名は破壊的変更として扱う。

### 3. カタログ

- `MsfPackaging` に `Catalog` を追加する。トップレベルのカタログトラック名の検証は維持する。
- `(packaging, role)` 別の必須フィールド表を実装し、`loc` / `video` と `loc` / `signlanguage` に `framerate`、`loc` / `audiodescription` に `lang` を要求する。role `data` を受理する。
- `packagingMetadata` を追加し、`;` 区切りの `key=value` を検証して `streamMapping` を取り出せるようにする。
- `depends` を String (track name) と Number (renderGroup) の enum にし、解決できない参照を拒否する。
- track-level `token` を削除する。key を `connectionURI` に変更する。`cipherSuite` の必須条件を `moq-secure-objects` に限定する。
- `accessibleWindowTime` を追加し、同じ `altGroup` の track で値が一致することを検証する。
- `initDataList` に `track-property` / `object-property` を追加し、type に応じて `data` を検証する。
- 既存のカタログ検証は v02 の MUST に合わせて更新する。v02 で緩和された項目 (initDataList の順序、`connectionUri`、`cipherSuite` 条件、version) を残さない。

### 4. delta update

- `"update"` 操作を追加する。update 用の部分更新型を新設し、`parentName` で対象を特定する。
- delta update に `generatedAt` を必須とし、`version` / `isComplete` / `tracks` / `publishTracks` / `initDataList` を拒否する。
- 再追加時の属性変更を拒否する v01 の不変検証を撤廃する。`isLive` の false から true への変更禁止は維持する。

### 5. タイムライン

- media timeline の media time 順を検証し、template 属性と explicit payload の併用を拒否する。
- media timeline / event timeline から `mimeType` 必須を外し、`depends` を v02 の条件 (media timeline は必須、event timeline は index が参照する場合のみ) に合わせる。renderGroup と個別 track の使い分け規則も検証する。
- Event Timeline の index key を `T` / `L` / `M` にし、`data` を任意の JSON 値にする。
- `MSF_EVENT_TIMELINE_HEADER` (0x7B) を Track property として定義し、payload 解析前の確認経路を用意する。

### 6. 伝送

- `streamMapping` の `group` / `object` を実装し、publisher がモードを選べるようにする。subscriber は両モードを処理する。既定は `group` とする。
- `keyframeSpacing` を追加し、alternate group 内で開始 Group ID を共有して 1 ずつ増やす publisher 側の割り当てを実装する。
- subscriber の multi-variant 切り替えを実装する。switch point は `(G * S + N) / T` が整数になる条件で判定し、target の開始 Group ID から Object ID 0 で受信する。
- カタログの publisher priority と subscriber priority を media より高くする。

### 7. 仕様参照と実装順序

- コードとドキュメントの MSF 参照を draft-02 の節番号に更新する。挙動は変えない。
- 実装順序は 1 (version は最後) → 2 → 3 → 4 → 5 → 6 → 7 とする。名前マッピング (2) はカタログ (3) と伝送 (6) の前提、`depends` の型 (3) は timeline (5) の前提、`keyframeSpacing` (6) は切り替え (6) の前提になる。

### 8. 仕様の矛盾

- `"update"` 操作は §5.1.6 が `parentName` を MUST とする一方、§5.2.3 はすべての track object に `name` を MUST とし、§5.6.4 の例は `name` を省略している。実装では update の track object で `name` を任意とし、`parentName` と矛盾する場合は拒否する。この判断は doc に残す。

## 完了条件

- `MSF_VERSION` が `"draft-02"` で、draft-02 のカタログを受理し、draft-01 と draft-03 を拒否すること。
- `/` 区切りの namespace が 1〜32 fields に変換され、空 field・区切り違反・NUL・BOM・未対サロゲートが拒否されること。`crates/moq-pub` の出力を `crates/moq-sub` が同じ `TrackNamespace` に解決すること。
- 変数値の `+` `/` `=` `.` が受理され、`,` `;` `"` `&` が拒否されること。`cat=` のトークンが SETUP と各制御メッセージに載り、`c4m` は予約として扱われないこと。
- `"catalog"` packaging、`(packaging, role)` 別必須フィールド、`packagingMetadata`、renderGroup を含む `depends`、`accessibleWindowTime`、`initDataList` の property 型が decode / encode で往復一致すること。
- `token` が出力されず、`connectionURI` が読み書きされ、`cipherSuite` が `moq-secure-objects` のときだけ必須になること。
- `"update"` 操作が適用でき、存在しない track の update と `isLive` の false から true が拒否されること。`generatedAt` 欠如と禁止されたルートフィールドが拒否されること。
- media timeline の media time 順違反と format 併用が拒否され、`mimeType` なしの timeline が受理されること。
- Event Timeline の `T` / `L` / `M` と任意 JSON の `data` が往復一致し、`MSF_EVENT_TIMELINE_HEADER` を取得できること。
- `streamMapping` の `group` / `object` が送受信でき、`object` モードで Subgroup の到着順が入れ替わっても Object を取りこぼさないこと。
- alternate group の `keyframeSpacing` と Group 割り当てが検証され、switch point の計算と切り替えが動作すること。
- カタログの publisher priority と subscriber priority が media より高いこと。
- `rg 'draft-ietf-moq-msf-01' src crates docs` が 0 件で、コード中の節番号が v02 と一致すること。
- `cargo test --workspace` / `cargo clippy --workspace --all-targets -- -D warnings` / `cargo fmt --all -- --check` が通ること。

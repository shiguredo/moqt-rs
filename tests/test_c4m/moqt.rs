//! C4M の `moqt` クレームと認可のテスト

use shiguredo_moqt::c4m::cat::{CAT_CONTENT_TYPE, CLAIM_EXPIRATION, CatClaims, CatError, CatToken};
use shiguredo_moqt::c4m::cbor::{self, Value};
use shiguredo_moqt::c4m::cose::{
    CoseEncodingOptions, CoseMac0, CoseMessage, HEADER_ALGORITHM, HEADER_TYPE,
};
use shiguredo_moqt::c4m::{
    C4mError, CLAIM_MOQT, CatDpop, MATCH_TYPE_PREFIX, MATCH_TYPE_SUFFIX, MOQT_SCOPES_KEY, Match,
    MoqtAction, MoqtClaim, MoqtScope, NamespaceMatch,
};

use super::helpers::{decode_hex, encode_hex};
use super::vectors::SCOPE_VECTORS;

/// draft-02 §2.1.2 の exact match の例の `moqt` クレーム値
///
/// EDN: `{0: [[[2, 3, 6, 7], ['example', 'com', nil], '/bob']]}` を決定論的
/// CBOR に落としたもので、encode の期待値にもなる。
const EXACT_MATCH_CLAIM_HEX: &str = "a1008183840203060783476578616d706c6543636f6df6442f626f62";

/// draft-02 §2.1.2 の prefix match の例の `moqt` クレーム値
///
/// EDN: `{0: [[[2, 3, 6, 7], ['example', 'com', nil], [1, '/bob']]]}` を決定論的
/// CBOR に落としたもので、encode の期待値にもなる。
const PREFIX_MATCH_CLAIM_HEX: &str = "a1008183840203060783476578616d706c6543636f6df68201442f626f62";

/// テスト用の namespace をバイト列の配列へ変換する
fn namespace_bytes<'a>(namespace: &'a [&'a str]) -> Vec<&'a [u8]> {
    namespace.iter().map(|field| field.as_bytes()).collect()
}

/// 認可判定を 1 件検証する
fn assert_authorization(
    claim: &MoqtClaim,
    action: i64,
    namespace: &[&str],
    track: &str,
    expected: bool,
) {
    let action = MoqtAction::from_key(action).expect("既知のアクションである");
    let namespace = namespace_bytes(namespace);
    assert_eq!(
        claim.authorize(action, &namespace, track.as_bytes()),
        expected,
        "認可判定が一致しない: action={action:?}, namespace={namespace:?}, track={track}"
    );
}

#[test]
fn scope_vectors_authorize_from_tokens() {
    for vector in SCOPE_VECTORS {
        let token = CatToken::decode(vector.token.as_bytes())
            .unwrap_or_else(|error| panic!("ベクタ {} のデコードに失敗した: {error}", vector.id));
        // claims の CBOR から直接デコードした場合も同じ結果になること
        let claims = CatClaims::decode(
            &cbor::decode(&decode_hex(vector.payload_hex)).expect("デコードできる"),
        )
        .expect("claims をデコードできる");
        assert_eq!(&claims, token.claims(), "ベクタ {}", vector.id);
        for test in vector.tests {
            let action = MoqtAction::from_key(test.action).unwrap_or_else(|| {
                panic!("ベクタ {} のアクションが不正: {}", vector.id, test.action)
            });
            let namespace = namespace_bytes(test.namespace);
            assert_eq!(
                token
                    .claims()
                    .authorize(action, &namespace, test.track.as_bytes()),
                test.expected,
                "ベクタ {} の認可判定 (action={}, namespace={:?}, track={})",
                vector.id,
                test.action,
                test.namespace,
                test.track
            );
        }
    }
}

#[test]
fn scope_vectors_have_expected_scopes() {
    let publisher = SCOPE_VECTORS
        .iter()
        .find(|vector| vector.id == "moqt_publisher_exact")
        .expect("ベクタがある");
    let claims = CatClaims::decode(
        &cbor::decode(&decode_hex(publisher.payload_hex)).expect("デコードできる"),
    )
    .expect("claims をデコードできる");
    let moqt = claims.moqt.as_ref().expect("moqt クレームがある");
    assert_eq!(moqt.scopes.len(), 1);
    let scope = &moqt.scopes[0];
    assert_eq!(scope.actions, [2, 6]);
    assert_eq!(
        scope.namespace,
        [
            NamespaceMatch::Match(Match::Exact(b"example.com".to_vec())),
            NamespaceMatch::Match(Match::Exact(b"alice".to_vec())),
        ]
    );
    assert_eq!(scope.track, Some(Match::Prefix(b"video-".to_vec())));

    let admin = SCOPE_VECTORS
        .iter()
        .find(|vector| vector.id == "moqt_admin_wildcard")
        .expect("ベクタがある");
    let claims =
        CatClaims::decode(&cbor::decode(&decode_hex(admin.payload_hex)).expect("デコードできる"))
            .expect("claims をデコードできる");
    let moqt = claims.moqt.as_ref().expect("moqt クレームがある");
    assert_eq!(moqt.scopes[0].actions, [0, 1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(moqt.scopes[0].namespace.is_empty());
    assert_eq!(moqt.scopes[0].track, None);
    // A.5 の admin ベクタは §2.1 Table 1 と不整合な旧 action 番号のままである。
    // 1 は SETUP の key なので SETUP を認可するが、9 (SUBSCRIBE_TRACKS) を含まない
    // ため SUBSCRIBE_TRACKS は認可しない
    assert!(claims.authorize(
        MoqtAction::Setup,
        &namespace_bytes(&["any.namespace"]),
        b"any-track"
    ));
    assert!(!claims.authorize(
        MoqtAction::SubscribeTracks,
        &namespace_bytes(&["any.namespace"]),
        b"any-track"
    ));
}

#[test]
fn scope_vectors_have_expected_moqt_reval() {
    for vector in SCOPE_VECTORS {
        let claims = CatClaims::decode(
            &cbor::decode(&decode_hex(vector.payload_hex)).expect("デコードできる"),
        )
        .expect("claims をデコードできる");
        assert_eq!(claims.moqt_reval, vector.moqt_reval, "ベクタ {}", vector.id);
    }
}

#[test]
fn token_with_provisional_claim_labels_has_typed_claims() {
    // draft-02 §A.5 の Private Use の暫定 label (-65537 / -65538) を使うベクタ
    let vector = SCOPE_VECTORS
        .iter()
        .find(|vector| vector.id == "moqt_multi_scope")
        .expect("ベクタがある");
    let token = CatToken::decode(vector.token.as_bytes()).expect("デコードできる");
    let claims = token.claims();

    // 暫定 label の moqt / moqt-reval が型付きフィールドへ取り込まれること
    assert!(claims.moqt.is_some(), "moqt が型付きでデコードされること");
    assert_eq!(
        claims.moqt_reval,
        Some(300.0),
        "moqt-reval が型付きでデコードされること"
    );
    // 型付きで解釈したため raw には残らないこと
    assert!(claims.raw.is_empty());

    // 新 label の token で scope 認可が機能すること
    assert!(claims.authorize(
        MoqtAction::Publish,
        &[b"live.example".as_slice(), b"studio-a".as_slice()],
        b"cam1",
    ));
    assert!(!claims.authorize(
        MoqtAction::Publish,
        &[b"live.example".as_slice(), b"studio-b".as_slice()],
        b"cam1",
    ));
}

#[test]
fn publisher_scope_rebuild_matches_vector_payload() {
    let vector = SCOPE_VECTORS
        .iter()
        .find(|vector| vector.id == "moqt_publisher_exact")
        .expect("ベクタがある");
    let claims = CatClaims {
        issuer: Some(String::from("https://auth.example.com")),
        expiration: Some(1700086400.0),
        moqt: Some(
            MoqtClaim::new().scope(
                MoqtScope::new([MoqtAction::PublishNamespace, MoqtAction::Publish])
                    .namespace_match(NamespaceMatch::Match(Match::Exact(b"example.com".to_vec())))
                    .namespace_match(NamespaceMatch::Match(Match::Exact(b"alice".to_vec())))
                    .track(Match::Prefix(b"video-".to_vec())),
            ),
        ),
        ..CatClaims::default()
    };
    assert_eq!(
        encode_hex(
            &cbor::encode(&claims.encode().expect("エンコードできる")).expect("エンコードできる")
        ),
        vector.payload_hex
    );
}

#[test]
fn nil_matches_only_the_end_of_the_namespace() {
    let scope = MoqtScope::new([MoqtAction::Subscribe])
        .namespace_match(NamespaceMatch::Match(Match::Exact(b"example.com".to_vec())))
        .namespace_end();
    assert!(scope.allows(MoqtAction::Subscribe, &[b"example.com".as_slice()], b""));
    assert!(!scope.allows(
        MoqtAction::Subscribe,
        &[b"example.com".as_slice(), b"alice".as_slice()],
        b""
    ));
    assert!(!scope.allows(MoqtAction::Subscribe, &[b"example".as_slice()], b""));
}

#[test]
fn without_nil_longer_namespaces_are_allowed() {
    let scope = MoqtScope::new([MoqtAction::Subscribe])
        .namespace_match(NamespaceMatch::Match(Match::Prefix(b"example".to_vec())));
    assert!(scope.allows(
        MoqtAction::Subscribe,
        &[b"example.com".as_slice(), b"alice".as_slice()],
        b""
    ));
    assert!(!scope.allows(MoqtAction::Subscribe, &[b"other".as_slice()], b""));
}

#[test]
fn scope_without_namespace_matches_any_namespace() {
    let scope = MoqtScope::new([MoqtAction::Publish]);
    assert!(scope.allows(MoqtAction::Publish, &[], b"track"));
    assert!(scope.allows(
        MoqtAction::Publish,
        &[b"any".as_slice(), b"namespace".as_slice()],
        b"track"
    ));
    assert!(!scope.allows(MoqtAction::Fetch, &[b"any".as_slice()], b"track"));
}

#[test]
fn track_match_is_applied_when_present() {
    let scope = MoqtScope::new([MoqtAction::Publish]).track(Match::Suffix(b".json".to_vec()));
    assert!(scope.allows(MoqtAction::Publish, &[b"a".as_slice()], b"data.json"));
    assert!(!scope.allows(MoqtAction::Publish, &[b"a".as_slice()], b"data.xml"));
    // 名前空間マッチが無くてもトラックマッチは適用される
    assert!(scope.allows(MoqtAction::Publish, &[], b".json"));
}

#[test]
fn decode_errors_for_invalid_scopes() {
    let decode_scope = |value: Value| MoqtScope::decode(&value);
    assert_eq!(
        decode_scope(Value::Array(vec![Value::Array(Vec::new())])),
        Err(C4mError::EmptyActions)
    );
    assert_eq!(
        decode_scope(Value::Array(Vec::new())),
        Err(C4mError::InvalidScopeLength(0))
    );
    assert_eq!(
        decode_scope(Value::Array(vec![
            Value::Array(vec![Value::integer(1)]),
            Value::Array(Vec::new()),
        ])),
        Err(C4mError::EmptyNamespaceMatch)
    );
    assert_eq!(
        decode_scope(Value::Array(vec![
            Value::Array(vec![Value::integer(1)]),
            Value::Array(vec![
                Value::Null,
                Value::Array(vec![Value::integer(1), Value::ByteString(b"a".to_vec())]),
            ]),
        ])),
        Err(C4mError::NilNotLast)
    );
    assert_eq!(
        decode_scope(Value::Array(vec![
            Value::Array(vec![Value::integer(1)]),
            Value::Array(vec![Value::Array(vec![
                Value::integer(3),
                Value::ByteString(b"a".to_vec()),
            ])]),
        ])),
        Err(C4mError::InvalidMatchType(3))
    );
    assert_eq!(
        decode_scope(Value::Array(vec![
            Value::Array(vec![Value::integer(1)]),
            Value::Array(vec![Value::Array(vec![Value::integer(1)])]),
        ])),
        Err(C4mError::InvalidMatchArrayLength(1))
    );
    assert_eq!(
        decode_scope(Value::Unsigned(1)),
        Err(C4mError::UnexpectedType("moqt-scope"))
    );
    assert_eq!(
        decode_scope(Value::Array(vec![Value::ByteString(b"a".to_vec())])),
        Err(C4mError::UnexpectedType("moqt-actions"))
    );
    assert_eq!(
        decode_scope(Value::Array(vec![Value::Array(vec![Value::TextString(
            String::from("a")
        ),])])),
        Err(C4mError::UnexpectedType("moqt-action"))
    );
    assert_eq!(
        decode_scope(Value::Array(vec![
            Value::Array(vec![Value::integer(1)]),
            Value::ByteString(b"a".to_vec()),
        ])),
        Err(C4mError::UnexpectedType("moqt-ns-match"))
    );
}

#[test]
fn encode_errors_and_round_trip() {
    let empty_actions = MoqtScope {
        actions: Vec::new(),
        namespace: Vec::new(),
        track: None,
    };
    assert_eq!(empty_actions.encode(), Err(C4mError::EmptyActions));
    // claim 経由でもスコープのエラーが伝播すること (fail-closed)
    assert_eq!(
        MoqtClaim::new().scope(empty_actions.clone()).encode(),
        Err(C4mError::EmptyActions),
        "claim の encode はスコープのエラーを握りつぶさないこと"
    );
    let nil_not_last = MoqtScope {
        actions: vec![1],
        namespace: vec![
            NamespaceMatch::End,
            NamespaceMatch::Match(Match::Exact(vec![1])),
        ],
        track: None,
    };
    assert_eq!(nil_not_last.encode(), Err(C4mError::NilNotLast));
    assert_eq!(MoqtClaim::default().encode(), Err(C4mError::EmptyScopes));

    let scope = MoqtScope::new([MoqtAction::Fetch])
        .namespace_match(NamespaceMatch::Match(Match::Prefix(b"live".to_vec())))
        .namespace_end()
        .track(Match::Suffix(b"-audio".to_vec()));
    let encoded = scope.encode().expect("エンコードできる");
    assert_eq!(MoqtScope::decode(&encoded).expect("デコードできる"), scope);

    let claim = MoqtClaim::new()
        .scope(MoqtScope::new([MoqtAction::Subscribe]))
        .scope(scope);
    let encoded = claim.encode().expect("エンコードできる");
    assert_eq!(MoqtClaim::decode(&encoded).expect("デコードできる"), claim);
}

#[test]
fn draft_02_exact_match_example_round_trips_and_authorizes() {
    // draft-02 §2.1.2 の exact match の例。EDN を CBOR に落とした既知のバイト列で
    // decode / encode / authorize が一致することを固定する
    let bytes = decode_hex(EXACT_MATCH_CLAIM_HEX);
    let claim = MoqtClaim::decode(&cbor::decode(&bytes).expect("CBOR をデコードできる"))
        .expect("map 形式の moqt クレームをデコードできる");
    assert_eq!(claim.scopes.len(), 1);
    assert_eq!(claim.scopes[0].actions, [2, 3, 6, 7]);
    assert_eq!(
        claim.scopes[0].namespace,
        [
            NamespaceMatch::Match(Match::Exact(b"example".to_vec())),
            NamespaceMatch::Match(Match::Exact(b"com".to_vec())),
            NamespaceMatch::End,
        ]
    );
    assert_eq!(claim.scopes[0].track, Some(Match::Exact(b"/bob".to_vec())));

    // encode は入力と同じ決定論的バイト列を返す
    assert_eq!(
        encode_hex(
            &cbor::encode(&claim.encode().expect("エンコードできる")).expect("エンコードできる")
        ),
        EXACT_MATCH_CLAIM_HEX
    );

    // §2.1.2 の Permits (スコープの全アクションで同じ Full Track Name を認可する)
    for action in [2, 3, 6, 7] {
        assert_authorization(&claim, action, &["example", "com"], "/bob", true);
    }
    // §2.1.2 の Prohibits。アクション、名前空間の長さ、トラック名のいずれかの
    // 違いで拒否される
    assert_authorization(&claim, 4, &["example", "com"], "/bob", false);
    for (namespace, track) in [
        (&["example", "com"][..], ""),
        (&["example", "com"][..], "/bob/123"),
        (&["example", "com"][..], "/alice"),
        (&["example", "com"][..], "/bob/logs"),
        (&["alternate", "example", "com"][..], "/bob"),
        (&["12345"][..], ""),
        (&["example"][..], "com/bob"),
        (&["example", "com", "/bob"][..], ""),
        (&["example", "com", ""][..], "/bob"),
    ] {
        assert_authorization(&claim, 6, namespace, track, false);
    }
}

#[test]
fn draft_02_prefix_match_example_round_trips_and_authorizes() {
    // draft-02 §2.1.2 の prefix match の例。exact match の例との差はトラック名の
    // マッチだけであり、map 形式の CBOR バイト列もその差だけになる
    let bytes = decode_hex(PREFIX_MATCH_CLAIM_HEX);
    let claim = MoqtClaim::decode(&cbor::decode(&bytes).expect("CBOR をデコードできる"))
        .expect("map 形式の moqt クレームをデコードできる");
    assert_eq!(claim.scopes[0].track, Some(Match::Prefix(b"/bob".to_vec())));
    assert_eq!(
        encode_hex(
            &cbor::encode(&claim.encode().expect("エンコードできる")).expect("エンコードできる")
        ),
        PREFIX_MATCH_CLAIM_HEX
    );

    // §2.1.2 の Permits (前方一致なのでトラック名の後続が自由になる)
    for track in ["/bob", "/bob/123", "/bob/logs"] {
        assert_authorization(&claim, 6, &["example", "com"], track, true);
    }
    // §2.1.2 の Prohibits
    for (namespace, track) in [
        (&["example", "com"][..], ""),
        (&["example", "com"][..], "/alice"),
        (&["alternate", "example", "com"][..], "/bob"),
        (&["12345"][..], ""),
        (&["example"][..], "com/bob"),
    ] {
        assert_authorization(&claim, 6, namespace, track, false);
    }
    // アクションの配列に無いアクションは、トラック名が一致しても拒否する
    assert_authorization(&claim, 4, &["example", "com"], "/bob", false);
}

#[test]
fn claim_encoding_is_deterministic_map_keyed_by_zero() {
    let claim = MoqtClaim::new()
        .scope(MoqtScope::new([MoqtAction::Publish]))
        .scope(
            MoqtScope::new([MoqtAction::Fetch])
                .namespace_match(NamespaceMatch::Match(Match::Prefix(b"example".to_vec()))),
        );
    let encoded = claim.encode().expect("エンコードできる");
    // claim value はキー 0 だけを持つ map であり、scopes 配列は既存の表現のまま
    let Value::Map(entries) = &encoded else {
        panic!("moqt クレームは map でなければならない: {encoded:?}");
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0, Value::integer(MOQT_SCOPES_KEY));
    assert_eq!(
        entries[0].1.as_array().expect("scopes は配列である").len(),
        2
    );

    // 決定論的バイト列は先頭がキー 0 の map (a1 00) になる
    let first = cbor::encode(&encoded).expect("エンコードできる");
    assert_eq!(&first[..2], &[0xa1, 0x00]);
    // encode → decode のラウンドトリップ
    assert_eq!(
        MoqtClaim::decode(&cbor::decode(&first).expect("デコードできる")).expect("デコードできる"),
        claim
    );

    // 単一スコープの最小の claim も期待どおりのバイト列になる
    let simple = MoqtClaim::new().scope(MoqtScope::new([MoqtAction::Publish]));
    assert_eq!(
        encode_hex(
            &cbor::encode(&simple.encode().expect("エンコードできる")).expect("エンコードできる")
        ),
        "a10081818106"
    );
}

#[test]
fn claim_decode_errors_follow_fail_closed_rules() {
    let scope = || Value::Array(vec![Value::Array(vec![Value::integer(6)])]);

    // 旧 draft-01 の配列だけの値は map ではないため型不一致のエラーになる。
    // 空の scopes や新形式の claim として誤受理しない
    let legacy = cbor::decode(&decode_hex(
        "8183820206824b6578616d706c652e636f6d45616c696365820146766964656f2d",
    ))
    .expect("CBOR をデコードできる");
    assert_eq!(
        MoqtClaim::decode(&legacy),
        Err(C4mError::UnexpectedType("moqt claim"))
    );
    // claims set 経由でも旧形式の claim は受理しない
    assert_eq!(
        CatClaims::decode(
            &cbor::decode(&claims_cbor(legacy)).expect("claims set の CBOR をデコードできる")
        ),
        Err(CatError::C4m(C4mError::UnexpectedType("moqt claim")))
    );

    // キー 0 の値が配列でない
    assert_eq!(
        MoqtClaim::decode(&Value::Map(vec![(Value::integer(0), Value::Unsigned(1))])),
        Err(C4mError::UnexpectedType("moqt-scopes"))
    );
    // scopes が空
    assert_eq!(
        MoqtClaim::decode(&Value::Map(vec![(
            Value::integer(0),
            Value::Array(Vec::new())
        )])),
        Err(C4mError::EmptyScopes)
    );
    // キー 0 の重複
    assert_eq!(
        MoqtClaim::decode(&Value::Map(vec![
            (Value::integer(0), scope()),
            (Value::integer(0), scope()),
        ])),
        Err(C4mError::DuplicateScopesKey)
    );
    // 整数でないキー
    assert_eq!(
        MoqtClaim::decode(&Value::Map(vec![(
            Value::TextString(String::from("scopes")),
            scope(),
        )])),
        Err(C4mError::UnexpectedType("moqt claim key"))
    );
    // キー 0 以外は将来の改訂のための予約であり、未知のキーは fail-closed で拒否する
    assert_eq!(
        MoqtClaim::decode(&Value::Map(vec![
            (Value::integer(0), scope()),
            (Value::integer(1), Value::Null),
        ])),
        Err(C4mError::UnknownMoqtClaimKey(1))
    );
    // キー 0 が無く未知のキーだけの場合も、空の scopes に読み替えない
    assert_eq!(
        MoqtClaim::decode(&Value::Map(vec![(Value::integer(2), Value::Null)])),
        Err(C4mError::UnknownMoqtClaimKey(2))
    );
    // キー 0 の値のスコープ要素が不正な場合、MoqtScope::decode のエラーが
    // そのまま伝播すること (空のスコープ配列は InvalidScopeLength)
    let invalid_scope = Value::Map(vec![(
        Value::integer(MOQT_SCOPES_KEY),
        Value::Array(vec![Value::Array(Vec::new())]),
    )]);
    assert_eq!(
        MoqtClaim::decode(&invalid_scope),
        Err(C4mError::InvalidScopeLength(0)),
        "スコープのデコードエラーをそのまま返すこと"
    );

    // i64 で表せない CBOR 整数キーは「解釈できないキー」として拒否すること
    for key in [Value::Unsigned(u64::MAX), Value::Negative(u64::MAX)] {
        assert_eq!(
            MoqtClaim::decode(&Value::Map(vec![
                (Value::integer(MOQT_SCOPES_KEY), Value::Array(vec![])),
                (key, Value::integer(0)),
            ])),
            Err(C4mError::UnexpectedType("moqt claim key")),
            "i64 で表せないキーは型エラーとして拒否すること"
        );
    }

    // エラー優先順: 未知キーが先に見つかれば UnknownMoqtClaimKey、キー 0 も未知キーも
    // 無い空 map だけが MissingScopesKey になること
    assert_eq!(
        MoqtClaim::decode(&Value::Map(vec![])),
        Err(C4mError::MissingScopesKey)
    );
    assert_eq!(
        MoqtClaim::decode(&Value::Map(vec![(Value::integer(1), Value::integer(0))])),
        Err(C4mError::UnknownMoqtClaimKey(1))
    );
}

#[test]
fn claims_set_decodes_moqt_claim_map() {
    // claims set の型付きクレーム経由でも map 形式がそのまま通ること
    let claim = MoqtClaim::new().scope(
        MoqtScope::new([MoqtAction::Subscribe])
            .namespace_match(NamespaceMatch::Match(Match::Exact(b"example.com".to_vec()))),
    );
    let claims = CatClaims {
        issuer: Some(String::from("https://auth.example.com")),
        moqt: Some(claim),
        ..CatClaims::default()
    };
    let payload =
        cbor::encode(&claims.encode().expect("エンコードできる")).expect("エンコードできる");
    let decoded = CatClaims::decode(&cbor::decode(&payload).expect("CBOR をデコードできる"))
        .expect("claims をデコードできる");
    assert_eq!(decoded, claims);
    // claims set 経由の認可も map 形式の scopes を評価する
    assert!(decoded.authorize(
        MoqtAction::Subscribe,
        &namespace_bytes(&["example.com"]),
        b"video"
    ));
}

/// `moqt` クレーム値を持つ claims set の CBOR を組み立てる
fn claims_cbor(moqt: Value) -> Vec<u8> {
    cbor::encode(&Value::Map(vec![
        (Value::integer(CLAIM_MOQT), moqt),
        (Value::integer(CLAIM_EXPIRATION), Value::integer(1700086400)),
    ]))
    .expect("エンコードできる")
}

/// claims set を COSE_Mac0 のトークンへ包む
///
/// 署名検証はしないため MAC は任意のバイト列でよい。
fn cose_mac0_token(claims: Vec<u8>) -> Vec<u8> {
    let protected = cbor::encode(&Value::Map(vec![
        (Value::integer(HEADER_ALGORITHM), Value::integer(5)),
        (
            Value::integer(HEADER_TYPE),
            Value::TextString(String::from(CAT_CONTENT_TYPE)),
        ),
    ]))
    .expect("エンコードできる");
    CoseMessage::Mac0(CoseMac0 {
        protected,
        unprotected: Vec::new(),
        payload: Some(claims),
        tag: vec![0; 32],
        cose_tagged: true,
        cwt_tagged: true,
    })
    .encode(&CoseEncodingOptions::default())
    .expect("エンコードできる")
}

#[test]
fn claims_set_with_unknown_moqt_claim_key_is_not_acceptable() {
    // 未知のキーを含む moqt claim の claim set は受理不可 (§2.1.1)
    let scope = Value::Array(vec![Value::Array(vec![Value::integer(
        MoqtAction::Publish.key(),
    )])]);
    let moqt = Value::Map(vec![
        (Value::integer(MOQT_SCOPES_KEY), Value::Array(vec![scope])),
        (Value::integer(1), Value::integer(0)),
    ]);
    assert_eq!(
        CatClaims::decode(&cbor::decode(&claims_cbor(moqt)).expect("CBOR をデコードできる")),
        Err(CatError::C4m(C4mError::UnknownMoqtClaimKey(1)))
    );
}

#[test]
fn token_with_unknown_moqt_claim_key_is_rejected() {
    // キー 1 を知らない制限として追加した token は claims のデコード時点で拒否され、
    // 認可まで到達しない。キー 1 を除いた同じ token は PUBLISH を認可するため、
    // 未知のキーを無視して認可すると制限が落ちるケースであることを固定する
    let scope = Value::Array(vec![Value::Array(vec![Value::integer(
        MoqtAction::Publish.key(),
    )])]);
    let known = claims_cbor(Value::Map(vec![(
        Value::integer(MOQT_SCOPES_KEY),
        Value::Array(vec![scope.clone()]),
    )]));
    let token =
        CatToken::decode(&cose_mac0_token(known)).expect("既知のキーだけならデコードできる");
    assert!(
        token
            .claims()
            .authorize(MoqtAction::Publish, &namespace_bytes(&["ns"]), b"track")
    );

    let unknown = claims_cbor(Value::Map(vec![
        (Value::integer(MOQT_SCOPES_KEY), Value::Array(vec![scope])),
        (Value::integer(1), Value::integer(0)),
    ]));
    let err = CatToken::decode(&cose_mac0_token(unknown))
        .expect_err("未知キーを含む token は拒否されること");
    assert_eq!(err, CatError::C4m(C4mError::UnknownMoqtClaimKey(1)));
}

#[test]
fn example_three_map_form_match_is_rejected_as_cddl_violation() {
    // draft-02 §2.1.2 の 3 番目の例は namespace match / track match を `{ 0: bstr }`
    // という map で書いているが、§2.1 の CDDL の bin-match は `bstr / [match-type,
    // match-value]` であり map ではない (例 1 / 2 / 4 以降は CDDL と整合する)。
    // 規範である CDDL を採り、例 3 の形は受け付けないことを固定する。
    let ns_match_map = Value::Map(vec![(
        Value::integer(0),
        Value::ByteString(b"example".to_vec()),
    )]);
    let track_match_map = Value::Map(vec![(
        Value::integer(0),
        Value::ByteString(b"/bob".to_vec()),
    )]);

    // namespace match の位置に map を置いたスコープ
    let ns_map_scope = Value::Array(vec![
        Value::Array(vec![Value::integer(2)]),
        Value::Array(vec![ns_match_map.clone()]),
        Value::ByteString(b"/bob".to_vec()),
    ]);
    assert_eq!(
        MoqtScope::decode(&ns_map_scope),
        Err(C4mError::UnexpectedType("bin-match")),
        "namespace match は map ではなく bin-match でなければならない"
    );

    // track match の位置に map を置いたスコープ
    let track_map_scope = Value::Array(vec![
        Value::Array(vec![Value::integer(2)]),
        Value::Array(vec![Value::ByteString(b"example".to_vec())]),
        track_match_map,
    ]);
    assert_eq!(
        MoqtScope::decode(&track_map_scope),
        Err(C4mError::UnexpectedType("bin-match")),
        "track match は map ではなく bin-match でなければならない"
    );

    // claim 全体としても受理しない
    let claim = Value::Map(vec![(
        Value::integer(MOQT_SCOPES_KEY),
        Value::Array(vec![ns_map_scope]),
    )]);
    assert_eq!(
        MoqtClaim::decode(&claim),
        Err(C4mError::UnexpectedType("bin-match")),
        "§2.1.2 の 3 番目の例の形は claim として受理しない"
    );
}

#[test]
fn scope_encode_omits_empty_optional_parts() {
    let action_only = MoqtScope::new([MoqtAction::Publish]);
    assert_eq!(
        action_only.encode().expect("エンコードできる"),
        Value::Array(vec![Value::Array(vec![Value::integer(6)])])
    );
    let with_namespace = action_only
        .clone()
        .namespace_match(NamespaceMatch::Match(Match::Exact(b"a".to_vec())));
    assert_eq!(
        with_namespace.encode().expect("エンコードできる"),
        Value::Array(vec![
            Value::Array(vec![Value::integer(6)]),
            Value::Array(vec![Value::ByteString(b"a".to_vec())]),
        ])
    );
    // namespace を省略して track だけを持つことはできない
    // (CDDL の位置指定で表現できず、トラック制限を落とすと認可が広がるため)
    let track_only = action_only.clone().track(Match::Exact(b"t".to_vec()));
    assert_eq!(track_only.encode(), Err(C4mError::TrackWithoutNamespace));
}

#[test]
fn action_mapping() {
    // draft-02 §2.1 Table 1 の key 1 〜 9 と ALL の並びが一致する
    for (index, action) in MoqtAction::ALL.iter().enumerate() {
        let key = index as i64 + 1;
        assert_eq!(action.key(), key);
        assert_eq!(MoqtAction::from_key(key), Some(*action));
        assert!(!action.name().is_empty());
        // 旧 JWT 形式の文字列を持つアクションだけが自身の文字列と一致する
        match action.authorization_context() {
            Some(context) => {
                assert!(!context.is_empty());
                assert!(action.matches_authorization_context(context));
            }
            None => assert!(!action.matches_authorization_context("SETUP")),
        }
    }
    assert_eq!(MoqtAction::Setup.key(), 1);
    assert_eq!(MoqtAction::Setup.name(), "SETUP");
    assert_eq!(MoqtAction::Setup.authorization_context(), Some("SETUP"));
    assert_eq!(MoqtAction::SubscribeTracks.key(), 9);
    assert_eq!(MoqtAction::SubscribeTracks.name(), "SUBSCRIBE_TRACKS");
    assert_eq!(MoqtAction::SubscribeTracks.authorization_context(), None);
    // 旧 ClientSetup=0 は既知アクションの alias にしない
    assert_eq!(MoqtAction::from_key(0), None);
    assert!(!MoqtAction::Setup.matches_authorization_context("PUB_NS"));
}

#[test]
fn setup_and_subscribe_tracks_known_bytes_round_trip() {
    // draft-02 §2.1 Table 1 の SETUP=1 と SUBSCRIBE_TRACKS=9 だけを持つ claim である
    // {0: [[[1]]]} と {0: [[[9]]]} の決定論的 CBOR バイト列で、decode / encode /
    // authorize が一致することを固定する
    for (hex, action) in [
        ("a10081818101", MoqtAction::Setup),
        ("a10081818109", MoqtAction::SubscribeTracks),
    ] {
        let bytes = decode_hex(hex);
        let claim = MoqtClaim::decode(&cbor::decode(&bytes).expect("CBOR をデコードできる"))
            .expect("claim をデコードできる");
        assert_eq!(claim.scopes.len(), 1, "{hex}");
        assert_eq!(claim.scopes[0].actions, [action.key()], "{hex}");

        // encode は入力と同じ決定論的バイト列に戻る
        assert_eq!(
            encode_hex(
                &cbor::encode(&claim.encode().expect("エンコードできる"))
                    .expect("エンコードできる")
            ),
            hex
        );

        // 名前空間 / トラックの制限が無いスコープなので、任意の Full Track Name を認可する
        assert!(claim.authorize(action, &namespace_bytes(&["example", "com"]), b"video"));

        // 自分のアクションだけを認可し、もう一方のアクションは認可しない
        let other = if action == MoqtAction::Setup {
            MoqtAction::SubscribeTracks
        } else {
            MoqtAction::Setup
        };
        assert!(!claim.authorize(other, &namespace_bytes(&["example", "com"]), b"video"));
    }
}

#[test]
fn scope_without_requested_action_is_rejected() {
    // Publish だけを認可する scope は SETUP も SUBSCRIBE_TRACKS も認可しない
    let namespace = namespace_bytes(&["example", "com"]);
    let claim = MoqtClaim::new().scope(MoqtScope::new([MoqtAction::Publish]));
    assert!(!claim.authorize(MoqtAction::Setup, &namespace, b"video"));
    assert!(!claim.authorize(MoqtAction::SubscribeTracks, &namespace, b"video"));
    assert!(claim.authorize(MoqtAction::Publish, &namespace, b"video"));

    // 複数スコープのいずれかが要求アクションを含めば認可する
    let claim = MoqtClaim::new()
        .scope(MoqtScope::new([MoqtAction::Publish]))
        .scope(MoqtScope::new([MoqtAction::SubscribeTracks]));
    assert!(claim.authorize(MoqtAction::SubscribeTracks, &namespace, b"video"));
    assert!(!claim.authorize(MoqtAction::Setup, &namespace, b"video"));
}

#[test]
fn unknown_action_keys_are_not_mapped() {
    // §2.1 Table 1 に無い整数と、旧 CLIENT_SETUP の 0 は既知のアクションに変換しない
    for key in [0, 10, -1, i64::MIN, i64::MAX] {
        assert_eq!(MoqtAction::from_key(key), None, "key={key}");
    }
}

#[test]
fn action_integer_out_of_i64_range_is_rejected() {
    // CBOR の整数は i64 の範囲を超え得るが、action は i64 として解釈できないため
    // 型エラーで claim 全体を拒否する (fail-closed)。符号なし / 負の両方の表現を含む
    for out_of_range in [Value::Unsigned(u64::MAX), Value::Negative(u64::MAX)] {
        let value = Value::Map(vec![(
            Value::integer(MOQT_SCOPES_KEY),
            Value::Array(vec![Value::Array(vec![Value::Array(vec![out_of_range])])]),
        )]);
        assert_eq!(
            MoqtClaim::decode(&value),
            Err(C4mError::UnexpectedType("moqt-action"))
        );
    }

    // i64 の端にある未知の action は保持され、wire 往復でも失われない
    for key in [i64::MIN, i64::MAX] {
        let claim = MoqtClaim {
            scopes: vec![MoqtScope {
                actions: vec![key],
                namespace: Vec::new(),
                track: None,
            }],
        };
        let wire =
            cbor::encode(&claim.encode().expect("エンコードできる")).expect("CBOR へ変換できる");
        let decoded = MoqtClaim::decode(&cbor::decode(&wire).expect("CBOR をデコードできる"))
            .expect("デコードできる");
        assert_eq!(decoded, claim, "key={key}");
    }
}

#[test]
fn unknown_action_key_in_scope_is_kept_and_does_not_authorize_setup() {
    // 旧 CLIENT_SETUP=0 だけを持つ claim である {0: [[[0]]]}。0 は既知のアクションでは
    // ないため MoqtAction には変換できないが、scope の action 整数としては保持される
    let bytes = decode_hex("a10081818100");
    let claim = MoqtClaim::decode(&cbor::decode(&bytes).expect("CBOR をデコードできる"))
        .expect("未知の action 整数を含む claim もデコードできる");
    assert_eq!(claim.scopes[0].actions, [0]);
    // 0 を SETUP (1) の alias にしないため、SETUP は認可されない
    assert!(!claim.authorize(
        MoqtAction::Setup,
        &namespace_bytes(&["example", "com"]),
        b"video"
    ));
    // 再エンコードしても未知の整数 0 はそのまま残る
    assert_eq!(
        encode_hex(
            &cbor::encode(&claim.encode().expect("エンコードできる")).expect("エンコードできる")
        ),
        "a10081818100"
    );
}

#[test]
fn legacy_authorization_context_table() {
    // 旧 JWT 形式の DPoP proof の action 文字列を厳密に固定する。draft-02 §4.2 の
    // CWT 形式は整数の action コードを使うため、この表は旧形式との互換のためだけに残す
    let table: [(MoqtAction, Option<&str>); 9] = [
        (MoqtAction::Setup, Some("SETUP")),
        (MoqtAction::PublishNamespace, Some("PUB_NS")),
        (MoqtAction::SubscribeNamespace, Some("SUB_NS")),
        (MoqtAction::Subscribe, Some("SUBSCRIBE")),
        (MoqtAction::RequestUpdate, Some("REQ_UPDATE")),
        (MoqtAction::Publish, Some("PUBLISH")),
        (MoqtAction::Fetch, Some("FETCH")),
        (MoqtAction::TrackStatus, Some("TRK_STATUS")),
        (MoqtAction::SubscribeTracks, None),
    ];
    for action in MoqtAction::ALL {
        let (_, expected) = table
            .iter()
            .find(|(candidate, _)| *candidate == action)
            .expect("表に全アクションがある");
        assert_eq!(action.authorization_context(), *expected, "{action:?}");
        // 表の全文字列について、一致するのはそのアクションだけである
        for (other, value) in table {
            let expected_match = other == action && value == *expected;
            match value {
                Some(value) => assert_eq!(
                    action.matches_authorization_context(value),
                    expected_match,
                    "{action:?} / {value}"
                ),
                None => assert!(
                    !action.matches_authorization_context("SUBSCRIBE_TRACKS"),
                    "{action:?} は旧形式の文字列を持たない"
                ),
            }
        }
    }
    // 旧 CLIENT_SETUP / SERVER_SETUP という文字列はどのアクションの識別子でもない
    for action in MoqtAction::ALL {
        assert!(!action.matches_authorization_context("CLIENT_SETUP"));
        assert!(!action.matches_authorization_context("SERVER_SETUP"));
    }
}

#[test]
fn match_matching_rules() {
    assert!(Match::Exact(b"abc".to_vec()).matches(b"abc"));
    assert!(!Match::Exact(b"abc".to_vec()).matches(b"abcd"));
    assert!(Match::Prefix(b"ab".to_vec()).matches(b"abc"));
    assert!(!Match::Prefix(b"ab".to_vec()).matches(b"b"));
    assert!(Match::Suffix(b"bc".to_vec()).matches(b"abc"));
    assert!(!Match::Suffix(b"bc".to_vec()).matches(b"b"));
    assert_eq!(MATCH_TYPE_PREFIX, 1);
    assert_eq!(MATCH_TYPE_SUFFIX, 2);
    assert_eq!(
        Match::Prefix(b"x".to_vec()).encode(),
        Value::Array(vec![
            Value::integer(MATCH_TYPE_PREFIX),
            Value::ByteString(b"x".to_vec()),
        ])
    );
    assert_eq!(
        Match::Suffix(b"x".to_vec()).encode(),
        Value::Array(vec![
            Value::integer(MATCH_TYPE_SUFFIX),
            Value::ByteString(b"x".to_vec()),
        ])
    );
    assert_eq!(
        Match::Exact(b"x".to_vec()).encode(),
        Value::ByteString(b"x".to_vec())
    );
}

#[test]
fn catdpop_decodes_windows_and_jti_settings() {
    let catdpop = CatDpop::decode(&Value::Map(vec![
        (Value::integer(0), Value::integer(60)),
        (Value::integer(1), Value::integer(1)),
    ]))
    .expect("デコードできる");
    assert_eq!(catdpop.window_seconds, Some(60.0));
    assert_eq!(catdpop.honor_jti, Some(true));
    assert!(catdpop.honors_jti());
    assert_eq!(catdpop.window_seconds_or(300.0), 60.0);

    let catdpop = CatDpop::decode(&Value::Map(vec![
        (Value::integer(0), Value::Float(300.0)),
        (Value::integer(1), Value::integer(0)),
    ]))
    .expect("デコードできる");
    assert_eq!(catdpop.window_seconds, Some(300.0));
    assert_eq!(catdpop.honor_jti, Some(false));
    assert!(!catdpop.honors_jti());

    let catdpop = CatDpop::decode(&Value::Map(vec![(Value::integer(0), Value::integer(120))]))
        .expect("デコードできる");
    assert_eq!(catdpop.window_seconds, Some(120.0));
    assert_eq!(catdpop.honor_jti, None);
    assert!(!catdpop.honors_jti());
    assert_eq!(catdpop.window_seconds_or(300.0), 120.0);

    let catdpop = CatDpop::decode(&Value::Map(vec![(
        Value::integer(9),
        Value::TextString(String::from("x")),
    )]))
    .expect("未知の設定は raw に保持する");
    assert_eq!(catdpop.raw, [(9, Value::TextString(String::from("x")))]);
}

#[test]
fn catdpop_errors_and_round_trip() {
    assert_eq!(
        CatDpop::decode(&Value::Unsigned(1)),
        Err(C4mError::UnexpectedType("catdpop"))
    );
    assert_eq!(
        CatDpop::decode(&Value::Map(vec![(Value::integer(0), Value::Bool(true))])),
        Err(C4mError::UnexpectedType("catdpop window"))
    );
    assert_eq!(
        CatDpop::decode(&Value::Map(vec![(
            Value::integer(1),
            Value::TextString(String::from("x")),
        )])),
        Err(C4mError::UnexpectedType("catdpop honor jti"))
    );
    assert_eq!(
        CatDpop::decode(&Value::Map(vec![(
            Value::TextString(String::from("x")),
            Value::integer(1),
        )])),
        Err(C4mError::UnexpectedType("catdpop label"))
    );

    let catdpop = CatDpop::new(300.0, true);
    let encoded = catdpop.encode().expect("エンコードできる");
    assert_eq!(CatDpop::decode(&encoded).expect("デコードできる"), catdpop);
    // ドラフトの例に合わせて 1 / 0 の整数で書く
    assert_eq!(
        encoded.map_get(&Value::integer(1)),
        Some(&Value::integer(1))
    );
}

#[test]
fn nil_end_still_applies_track_match() {
    // nil で名前空間の長さを固定してもトラックマッチは評価される
    let scope = MoqtScope::new([MoqtAction::Publish])
        .namespace_match(NamespaceMatch::Match(Match::Exact(b"a".to_vec())))
        .namespace_end()
        .track(Match::Exact(b"t".to_vec()));
    assert!(scope.allows(MoqtAction::Publish, &[b"a".as_slice()], b"t"));
    assert!(!scope.allows(MoqtAction::Publish, &[b"a".as_slice()], b"other"));
    assert!(!scope.allows(
        MoqtAction::Publish,
        &[b"a".as_slice(), b"b".as_slice()],
        b"t"
    ));
}

#[test]
fn catdpop_non_finite_window_is_rejected() {
    assert_eq!(
        CatDpop::decode(&Value::Map(vec![(
            Value::integer(0),
            Value::Float(f64::NAN)
        )])),
        Err(C4mError::NonFiniteNumber("catdpop window"))
    );
}

#[test]
fn number_value_keeps_two_to_the_63() {
    // 2^63 は i64 に収まらないため、静かに 2^63-1 へ丸めず浮動小数点数のまま扱う
    let value = 2f64.powi(63);
    let catdpop = CatDpop {
        window_seconds: Some(value),
        honor_jti: None,
        raw: Vec::new(),
    };
    let encoded = catdpop.encode().expect("エンコードできる");
    let decoded = CatDpop::decode(&encoded).expect("デコードできる");
    assert_eq!(decoded.window_seconds, Some(value));
}

#[test]
fn allows_rejects_nil_in_the_middle() {
    // デコードでは弾かれるが、直接構築したスコープでも nil の位置異常は認可しない
    let scope = MoqtScope {
        actions: vec![MoqtAction::Publish.key()],
        namespace: vec![
            NamespaceMatch::Match(Match::Exact(b"a".to_vec())),
            NamespaceMatch::End,
            NamespaceMatch::Match(Match::Exact(b"b".to_vec())),
        ],
        track: None,
    };
    assert!(!scope.allows(
        MoqtAction::Publish,
        &[b"a".as_slice(), b"b".as_slice()],
        b"t"
    ));
    assert!(!scope.allows(MoqtAction::Publish, &[b"a".as_slice()], b"t"));
}

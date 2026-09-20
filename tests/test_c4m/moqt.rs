//! C4M の `moqt` クレームと認可のテスト

use shiguredo_moqt::c4m::cat::{CatClaims, CatToken};
use shiguredo_moqt::c4m::cbor::{self, Value};
use shiguredo_moqt::c4m::{
    C4mError, CatDpop, MATCH_TYPE_PREFIX, MATCH_TYPE_SUFFIX, Match, MoqtAction, MoqtClaim,
    MoqtScope, NamespaceMatch,
};

use super::helpers::{decode_hex, encode_hex};
use super::vectors::SCOPE_VECTORS;

/// テスト用の namespace をバイト列の配列へ変換する
fn namespace_bytes<'a>(namespace: &'a [&'a str]) -> Vec<&'a [u8]> {
    namespace.iter().map(|field| field.as_bytes()).collect()
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
    assert_eq!(
        MoqtClaim::decode(&Value::Array(Vec::new())),
        Err(C4mError::EmptyScopes)
    );
    assert_eq!(
        MoqtClaim::decode(&Value::Unsigned(1)),
        Err(C4mError::UnexpectedType("moqt claim"))
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
    for (index, action) in MoqtAction::ALL.iter().enumerate() {
        assert_eq!(action.key(), index as i64);
        assert_eq!(MoqtAction::from_key(index as i64), Some(*action));
        assert!(!action.name().is_empty());
        assert!(!action.authorization_context().is_empty());
        assert!(action.matches_authorization_context(action.authorization_context()));
    }
    assert_eq!(MoqtAction::from_key(9), None);
    assert_eq!(MoqtAction::ClientSetup.name(), "CLIENT_SETUP");
    assert_eq!(MoqtAction::ClientSetup.authorization_context(), "SETUP");
    assert_eq!(MoqtAction::ServerSetup.authorization_context(), "SETUP");
    assert!(!MoqtAction::ClientSetup.matches_authorization_context("PUB_NS"));
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

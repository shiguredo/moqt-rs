//! C4M の property テストで共有する生成器とモデル

use pbt::common::sample_bytes;
use shiguredo_moqt::c4m::{CatDpop, Match, MoqtAction, MoqtClaim, MoqtScope, NamespaceMatch};

/// 仕様 (draft-ietf-moq-c4m-01 §2.1) のマッチ規則をそのまま実装したモデル
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelMatch {
    /// 完全一致
    Exact(Vec<u8>),
    /// 前方一致
    Prefix(Vec<u8>),
    /// 後方一致
    Suffix(Vec<u8>),
}

impl ModelMatch {
    /// 値がマッチするかどうかを返す
    pub fn matches(&self, value: &[u8]) -> bool {
        match self {
            Self::Exact(pattern) => value == pattern.as_slice(),
            Self::Prefix(pattern) => value.starts_with(pattern),
            Self::Suffix(pattern) => value.ends_with(pattern),
        }
    }

    /// SUT の [`Match`] へ変換する
    pub fn to_match(&self) -> Match {
        match self {
            Self::Exact(pattern) => Match::Exact(pattern.clone()),
            Self::Prefix(pattern) => Match::Prefix(pattern.clone()),
            Self::Suffix(pattern) => Match::Suffix(pattern.clone()),
        }
    }
}

/// 仕様の規則を独立に計算するためのスコープのモデル
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelScope {
    /// 認可するアクション
    pub actions: Vec<i64>,
    /// 名前空間フィールドのマッチ
    pub namespace: Vec<ModelMatch>,
    /// 名前空間の末尾を `nil` で固定するかどうか
    pub ends_with_nil: bool,
    /// トラック名のマッチ
    pub track: Option<ModelMatch>,
}

impl ModelScope {
    /// 仕様の規則で認可を判定する (SUT とは独立の実装)
    pub fn allows(&self, action: MoqtAction, namespace: &[&[u8]], track: &[u8]) -> bool {
        if !self.actions.contains(&action.key()) {
            return false;
        }
        if namespace.len() < self.namespace.len() {
            return false;
        }
        if self.ends_with_nil && namespace.len() != self.namespace.len() {
            return false;
        }
        for (matcher, field) in self.namespace.iter().zip(namespace) {
            if !matcher.matches(field) {
                return false;
            }
        }
        match &self.track {
            Some(matcher) => matcher.matches(track),
            None => true,
        }
    }

    /// SUT の [`MoqtScope`] へ変換する
    pub fn to_scope(&self) -> MoqtScope {
        let actions = self
            .actions
            .iter()
            .filter_map(|action| MoqtAction::from_key(*action));
        let mut scope = MoqtScope::new(actions);
        for matcher in &self.namespace {
            scope = scope.namespace_match(NamespaceMatch::Match(matcher.to_match()));
        }
        if self.ends_with_nil {
            scope = scope.namespace_end();
        }
        if let Some(track) = &self.track {
            scope = scope.track(track.to_match());
        }
        scope
    }
}

/// 短いバイト列を生成する
fn sample_short_bytes(ctx: &mut noprop::TestCaseContext) -> Vec<u8> {
    sample_bytes(ctx, 8)
}

/// マッチを生成する
pub fn sample_match(ctx: &mut noprop::TestCaseContext) -> ModelMatch {
    let pattern = sample_short_bytes(ctx);
    match noprop::sample_weighted_index(ctx, &[3, 2, 2]) {
        0 => ModelMatch::Exact(pattern),
        1 => ModelMatch::Prefix(pattern),
        _ => ModelMatch::Suffix(pattern),
    }
}

/// 認可対象に寄せたスコープを生成する
///
/// アクション・名前空間・トラックの一部を対象からコピーすることで、認可される
/// ケースと拒否されるケースの両方を高い確率で生成する。
pub fn sample_model_scope(
    ctx: &mut noprop::TestCaseContext,
    action: MoqtAction,
    namespace: &[Vec<u8>],
    track: &[u8],
) -> ModelScope {
    // アクションは対象を含むか、含まない別のアクションにする
    let actions = if noprop::sample_ratio(ctx, noprop::Ratio::one_nth(2)) {
        vec![action.key()]
    } else {
        vec![(action.key() + 1) % 9]
    };

    // 名前空間マッチは対象の先頭から切り出すか、対象より 1 つ長くする
    let max_matches = namespace.len() + 1;
    let match_count = noprop::sample_usize_in(ctx, 0..=max_matches);
    let mut namespace_matches = Vec::new();
    for index in 0..match_count {
        if let Some(field) = namespace.get(index) {
            let mut candidates = vec![
                ModelMatch::Exact(field.clone()),
                ModelMatch::Prefix(field.clone()),
                ModelMatch::Suffix(field.clone()),
            ];
            candidates.push(sample_match(ctx));
            let choice = noprop::sample_usize_in(ctx, 0..candidates.len());
            namespace_matches.push(candidates[choice].clone());
        } else {
            namespace_matches.push(sample_match(ctx));
        }
    }

    // 名前空間の末尾固定は半分の確率で行う
    let ends_with_nil = noprop::sample_ratio(ctx, noprop::Ratio::one_nth(2));

    // トラックマッチは半分の確率で対象から作る。名前空間マッチが 1 つも無い場合は
    // CDDL の位置指定で表現できないため生成しない
    let track_match =
        if namespace_matches.is_empty() || noprop::sample_ratio(ctx, noprop::Ratio::one_nth(2)) {
            None
        } else {
            let candidates = [
                ModelMatch::Exact(track.to_vec()),
                ModelMatch::Prefix(track.to_vec()),
                ModelMatch::Suffix(track.to_vec()),
                sample_match(ctx),
            ];
            let choice = noprop::sample_usize_in(ctx, 0..candidates.len());
            Some(candidates[choice].clone())
        };

    ModelScope {
        actions,
        namespace: namespace_matches,
        ends_with_nil,
        track: track_match,
    }
}

/// 妥当なスコープを生成する (クレームのラウンドトリップ用)
pub fn sample_scope(ctx: &mut noprop::TestCaseContext) -> MoqtScope {
    let action_count = noprop::sample_usize_in(ctx, 1..=3);
    let action = MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())];
    let namespace = (0..noprop::sample_usize_in(ctx, 0..=3))
        .map(|_| sample_short_bytes(ctx))
        .collect::<Vec<_>>();
    let track = sample_short_bytes(ctx);
    let mut model = sample_model_scope(ctx, action, &namespace, &track);
    model.actions = (0..action_count)
        .map(|_| MoqtAction::ALL[noprop::sample_usize_in(ctx, 0..MoqtAction::ALL.len())].key())
        .collect();
    model.to_scope()
}

/// `moqt` クレームを生成する
pub fn sample_moqt_claim(ctx: &mut noprop::TestCaseContext) -> MoqtClaim {
    let scope_count = noprop::sample_usize_in(ctx, 1..=3);
    let mut claim = MoqtClaim::new();
    for _ in 0..scope_count {
        claim = claim.scope(sample_scope(ctx));
    }
    claim
}

/// `catdpop` クレームを生成する
pub fn sample_catdpop(ctx: &mut noprop::TestCaseContext) -> CatDpop {
    let window = if noprop::sample_bool(ctx) {
        Some(noprop::sample_f64_in(ctx, 1.0, 3600.0))
    } else {
        None
    };
    let honor_jti = if noprop::sample_bool(ctx) {
        Some(noprop::sample_bool(ctx))
    } else {
        None
    };
    // 未知の label は昇順にする (エンコード順とデコード順を一致させるため)
    let mut raw = Vec::new();
    for label in 2..noprop::sample_usize_in(ctx, 2..=4) {
        raw.push((
            label as i64,
            shiguredo_moqt::c4m::cbor::Value::Unsigned(noprop::sample_u64(ctx)),
        ));
    }
    CatDpop {
        window_seconds: window,
        honor_jti,
        raw,
    }
}

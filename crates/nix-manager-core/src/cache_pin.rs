//! Read-only cache-pin evidence shared by update planners and consumers.
//!
//! This is a data protocol, not a cache resolver or an apply engine. A cache
//! resolver produces one [`CachePinPlan`] per pin; consumers can inspect the
//! revision policy, package provenance, wish decisions, and aggregate gate
//! without reproducing those policies or touching repository files.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Version of the cache-pin evidence payload.
pub const PROTOCOL_SCHEMA_VERSION: u32 = 1;

fn default_protocol_version() -> u32 {
    PROTOCOL_SCHEMA_VERSION
}

/// Ordered availability ladder for one package at one revision.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Availability {
    #[default]
    Unknown,
    NotObserved,
    ObservedOnHydra,
    SubstitutableOutput,
    SubstitutableClosure,
    PromotionEligible,
}

impl Availability {
    /// Whether the evidence query failed or could not establish a result.
    pub fn is_unknown(self) -> bool {
        matches!(self, Self::Unknown)
    }

    /// Whether a configured cache can substitute the package output.
    pub fn is_substitutable(self) -> bool {
        matches!(
            self,
            Self::SubstitutableOutput | Self::SubstitutableClosure | Self::PromotionEligible
        )
    }

    /// Whether the evidence is sufficient to promote a wish package.
    pub fn is_promotion_eligible(self) -> bool {
        matches!(self, Self::PromotionEligible)
    }
}

/// Source of a package's public cache evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProvenanceSource {
    Hydra,
    NixEvaluation,
    BinaryCache,
    ClosureCheck,
}

/// Public, non-secret provenance for one availability observation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheProvenance {
    pub source: ProvenanceSource,
    #[serde(default)]
    pub cache_url: Option<String>,
    #[serde(default)]
    pub store_path: Option<String>,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
}

/// Compatibility proof attached to a package observation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Compatibility {
    #[default]
    Unknown,
    Compatible,
    Incompatible,
}

/// Availability and provenance for one required or wished package.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PackageEvidence {
    pub package: String,
    #[serde(default)]
    pub availability: Availability,
    #[serde(default)]
    pub compatibility: Compatibility,
    #[serde(default)]
    pub provenance: Vec<CacheProvenance>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// Git relationship and policy for the current and proposed revisions.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RevisionRelation {
    Equal,
    Newer,
    Older,
    Divergent,
    #[default]
    Unknown,
}

impl RevisionRelation {
    pub fn policy(self) -> RevisionPolicy {
        match self {
            Self::Equal | Self::Newer => RevisionPolicy::Allow,
            Self::Older => RevisionPolicy::DowngradeOverrideRequired,
            Self::Divergent => RevisionPolicy::DivergentHistory,
            Self::Unknown => RevisionPolicy::OrderingUnknown,
        }
    }

    pub fn blocks_without_override(self) -> bool {
        !matches!(self, Self::Equal | Self::Newer)
    }
}

/// Explicit policy result for a revision comparison.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RevisionPolicy {
    Allow,
    DowngradeOverrideRequired,
    DivergentHistory,
    #[default]
    OrderingUnknown,
}

/// Current/candidate revision evidence for a pin.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionEvidence {
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub candidate: Option<String>,
    #[serde(default)]
    pub relation: RevisionRelation,
    #[serde(default)]
    pub policy: RevisionPolicy,
}

impl RevisionEvidence {
    pub fn new(
        current: Option<String>,
        candidate: Option<String>,
        relation: RevisionRelation,
    ) -> Self {
        Self {
            current,
            candidate,
            relation,
            policy: relation.policy(),
        }
    }

    pub fn blocks_without_override(&self) -> bool {
        self.relation.blocks_without_override() || self.policy != self.relation.policy()
    }
}

/// Decision made for a configured wish package.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WishDecision {
    Continue,
    CurrentPinPromotionRequired,
    PromotionRequired,
    #[default]
    Unknown,
    Incompatible,
}

/// Evidence for a wish at the current pin and optional candidate revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WishEvidence {
    pub package: String,
    pub current: PackageEvidence,
    #[serde(default)]
    pub candidate: Option<PackageEvidence>,
    #[serde(default)]
    pub decision: WishDecision,
    #[serde(default)]
    pub reason: Option<String>,
}

impl WishEvidence {
    /// Apply the wish policy without querying or mutating anything.
    pub fn policy_decision(&self) -> WishDecision {
        if self.current.compatibility == Compatibility::Incompatible
            || self
                .candidate
                .as_ref()
                .is_some_and(|evidence| evidence.compatibility == Compatibility::Incompatible)
        {
            return WishDecision::Incompatible;
        }

        if self.current.availability.is_unknown()
            || self
                .candidate
                .as_ref()
                .is_some_and(|evidence| evidence.availability.is_unknown())
            || self.candidate.as_ref().is_some_and(|evidence| {
                evidence.availability.is_promotion_eligible()
                    && evidence.compatibility == Compatibility::Unknown
            })
        {
            return WishDecision::Unknown;
        }

        if self.current.availability.is_substitutable() {
            return WishDecision::CurrentPinPromotionRequired;
        }

        if self
            .candidate
            .as_ref()
            .is_some_and(|evidence| evidence.availability.is_promotion_eligible())
        {
            return WishDecision::PromotionRequired;
        }

        WishDecision::Continue
    }

    pub fn blocks_plan(&self) -> bool {
        !matches!(self.policy_decision(), WishDecision::Continue)
    }
}

/// Stable diagnostic identifiers for the read-only protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticCode {
    #[serde(rename = "update.availability.unknown")]
    AvailabilityUnknown,
    #[serde(rename = "update.cache.required-miss")]
    RequiredPackageUnavailable,
    #[serde(rename = "update.policy.package-incompatible")]
    PackageIncompatible,
    #[serde(rename = "update.action-required.wish-promotion")]
    WishPromotion,
    #[serde(rename = "update.availability.wish-unknown")]
    WishAvailabilityUnknown,
    #[serde(rename = "update.policy.wish-incompatible")]
    WishIncompatible,
    #[serde(rename = "update.policy.downgrade")]
    Downgrade,
    #[serde(rename = "update.policy.divergent-history")]
    DivergentHistory,
    #[serde(rename = "update.policy.revision-order-unknown")]
    RevisionOrderUnknown,
    #[serde(rename = "update.aggregate.failure")]
    AggregateFailure,
}

/// Diagnostic severity shared by pin and aggregate reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

/// Typed, public reason for a plan outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: DiagnosticCode,
    pub severity: DiagnosticSeverity,
    pub reason: String,
}

/// Outcome of resolving one pin.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PinStatus {
    Ready,
    NoChange,
    ActionRequired,
    Blocked,
    #[default]
    Failed,
}

impl PinStatus {
    pub fn blocks_aggregate(self) -> bool {
        matches!(self, Self::ActionRequired | Self::Blocked | Self::Failed)
    }
}

/// Read-only evidence and policy outcome for one cache pin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePinPlan {
    #[serde(default = "default_protocol_version")]
    pub schema_version: u32,
    pub pin: String,
    pub input_name: String,
    #[serde(default)]
    pub revision: RevisionEvidence,
    #[serde(default)]
    pub required_packages: Vec<PackageEvidence>,
    #[serde(default)]
    pub wishes: Vec<WishEvidence>,
    #[serde(default)]
    pub status: PinStatus,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
}

impl CachePinPlan {
    /// Build a policy result from resolver evidence without writing anything.
    pub fn evaluate(
        pin: impl Into<String>,
        input_name: impl Into<String>,
        revision: RevisionEvidence,
        required_packages: Vec<PackageEvidence>,
        mut wishes: Vec<WishEvidence>,
    ) -> Self {
        let mut diagnostics = Vec::new();
        let mut status = if revision.relation == RevisionRelation::Equal {
            PinStatus::NoChange
        } else {
            PinStatus::Ready
        };

        if revision.blocks_without_override() {
            let (code, reason) = match revision.relation {
                RevisionRelation::Older => (
                    DiagnosticCode::Downgrade,
                    "candidate revision is older than the current pin; an exact pin-scoped downgrade override is required".to_string(),
                ),
                RevisionRelation::Divergent => (
                    DiagnosticCode::DivergentHistory,
                    "candidate revision has divergent history from the current pin".to_string(),
                ),
                RevisionRelation::Unknown => (
                    DiagnosticCode::RevisionOrderUnknown,
                    "candidate revision ordering is unknown; refusing to infer monotonicity".to_string(),
                ),
                RevisionRelation::Equal | RevisionRelation::Newer => unreachable!(),
            };
            diagnostics.push(Diagnostic {
                code,
                severity: DiagnosticSeverity::Error,
                reason,
            });
            status = PinStatus::Blocked;
        }

        for package in &required_packages {
            if package.compatibility == Compatibility::Incompatible {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::PackageIncompatible,
                    severity: DiagnosticSeverity::Error,
                    reason: package.reason.clone().unwrap_or_else(|| {
                        format!(
                            "required package {} is incompatible with the pin",
                            package.package
                        )
                    }),
                });
                status = PinStatus::Blocked;
            } else if package.availability.is_unknown() {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::AvailabilityUnknown,
                    severity: DiagnosticSeverity::Error,
                    reason: format!(
                        "required package {} has unknown cache availability",
                        package.package
                    ),
                });
                status = PinStatus::Blocked;
            } else if !package.availability.is_substitutable() {
                diagnostics.push(Diagnostic {
                    code: DiagnosticCode::RequiredPackageUnavailable,
                    severity: DiagnosticSeverity::Error,
                    reason: package.reason.clone().unwrap_or_else(|| {
                        format!("required package {} is not substitutable", package.package)
                    }),
                });
                status = PinStatus::Blocked;
            }
        }

        for wish in &mut wishes {
            wish.decision = wish.policy_decision();
            let diagnostic = match wish.decision {
                WishDecision::Continue => None,
                WishDecision::CurrentPinPromotionRequired | WishDecision::PromotionRequired => {
                    if status != PinStatus::Blocked {
                        status = PinStatus::ActionRequired;
                    }
                    Some(Diagnostic {
                        code: DiagnosticCode::WishPromotion,
                        severity: DiagnosticSeverity::Warning,
                        reason: wish.reason.clone().unwrap_or_else(|| {
                            format!("wish package {} is ready for promotion", wish.package)
                        }),
                    })
                }
                WishDecision::Unknown => {
                    status = PinStatus::Blocked;
                    Some(Diagnostic {
                        code: DiagnosticCode::WishAvailabilityUnknown,
                        severity: DiagnosticSeverity::Error,
                        reason: wish.reason.clone().unwrap_or_else(|| {
                            format!("wish package {} has unknown availability", wish.package)
                        }),
                    })
                }
                WishDecision::Incompatible => {
                    status = PinStatus::Blocked;
                    Some(Diagnostic {
                        code: DiagnosticCode::WishIncompatible,
                        severity: DiagnosticSeverity::Error,
                        reason: wish.reason.clone().unwrap_or_else(|| {
                            format!("wish package {} is incompatible with the pin", wish.package)
                        }),
                    })
                }
            };
            if let Some(diagnostic) = diagnostic {
                diagnostics.push(diagnostic);
            }
        }

        Self {
            schema_version: PROTOCOL_SCHEMA_VERSION,
            pin: pin.into(),
            input_name: input_name.into(),
            revision,
            required_packages,
            wishes,
            status,
            diagnostics,
        }
    }

    pub fn is_blocking(&self) -> bool {
        self.status.blocks_aggregate()
            || self.revision.blocks_without_override()
            || self.required_packages.iter().any(|package| {
                package.compatibility == Compatibility::Incompatible
                    || package.availability.is_unknown()
                    || !package.availability.is_substitutable()
            })
            || self.wishes.iter().any(WishEvidence::blocks_plan)
    }
}

/// Aggregate state for a multi-pin read-only resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AggregateStatus {
    Ready,
    #[default]
    Blocked,
}

/// One pin-level reason that prevents the aggregate from proceeding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregateFailure {
    pub pin: String,
    pub code: DiagnosticCode,
    pub reason: String,
}

/// All pin results and the fail-before-write gate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePinAggregate {
    #[serde(default = "default_protocol_version")]
    pub schema_version: u32,
    pub pins: BTreeMap<String, CachePinPlan>,
    #[serde(default)]
    pub failures: Vec<AggregateFailure>,
    #[serde(default)]
    pub status: AggregateStatus,
}

impl CachePinAggregate {
    /// Combine independent pin results without starting any write operation.
    pub fn from_plans(plans: impl IntoIterator<Item = CachePinPlan>) -> Self {
        let mut pins = BTreeMap::new();
        let mut failures = Vec::new();

        for plan in plans {
            if plan.is_blocking() {
                if plan.diagnostics.is_empty() {
                    failures.push(AggregateFailure {
                        pin: plan.pin.clone(),
                        code: DiagnosticCode::AggregateFailure,
                        reason: "pin resolution failed; no cache-pin writes are permitted"
                            .to_string(),
                    });
                } else {
                    failures.extend(plan.diagnostics.iter().map(|diagnostic| AggregateFailure {
                        pin: plan.pin.clone(),
                        code: diagnostic.code,
                        reason: diagnostic.reason.clone(),
                    }));
                }
            }
            pins.insert(plan.pin.clone(), plan);
        }

        Self {
            schema_version: PROTOCOL_SCHEMA_VERSION,
            pins,
            status: if failures.is_empty() {
                AggregateStatus::Ready
            } else {
                AggregateStatus::Blocked
            },
            failures,
        }
    }

    pub fn is_ready(&self) -> bool {
        self.status == AggregateStatus::Ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVENANCE_FIXTURE: &str =
        include_str!("../tests/fixtures/cache-pin/cache-provenance.json");
    const POLICY_FIXTURE: &str =
        include_str!("../tests/fixtures/cache-pin/incompatible-downgrade.json");
    const AGGREGATE_FIXTURE: &str =
        include_str!("../tests/fixtures/cache-pin/aggregate-failure.json");

    #[test]
    fn cache_provenance_round_trips_without_losing_source_details() {
        let plan: CachePinPlan = serde_json::from_str(PROVENANCE_FIXTURE).unwrap();
        let evidence = &plan.required_packages[0];
        let provenance = evidence
            .provenance
            .iter()
            .find(|item| item.source == ProvenanceSource::BinaryCache)
            .unwrap();

        assert_eq!(evidence.availability, Availability::SubstitutableClosure);
        assert_eq!(
            provenance.cache_url.as_deref(),
            Some("https://cache.nixos.org")
        );
        assert_eq!(provenance.revision.as_deref(), Some("candidate-revision"));
        assert_eq!(provenance.store_path.as_deref(), Some("/nix/store/blender"));

        let encoded = serde_json::to_value(plan).unwrap();
        assert_eq!(encoded["schemaVersion"], PROTOCOL_SCHEMA_VERSION);
        assert_eq!(
            encoded["requiredPackages"][0]["provenance"][1]["cacheUrl"],
            "https://cache.nixos.org"
        );
    }

    #[test]
    fn downgrade_and_incompatible_wishes_are_blocking_policy() {
        let plan: CachePinPlan = serde_json::from_str(POLICY_FIXTURE).unwrap();

        assert_eq!(
            plan.revision.policy,
            RevisionPolicy::DowngradeOverrideRequired
        );
        assert!(plan.revision.blocks_without_override());
        assert!(plan.is_blocking());
        assert!(plan
            .wishes
            .iter()
            .any(|wish| wish.decision == WishDecision::Incompatible));
        assert!(plan
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == DiagnosticCode::Downgrade));
    }

    #[test]
    fn aggregate_failure_is_reported_before_any_write_boundary() {
        let plans: Vec<CachePinPlan> = serde_json::from_str(AGGREGATE_FIXTURE).unwrap();
        let aggregate = CachePinAggregate::from_plans(plans);

        assert!(!aggregate.is_ready());
        assert_eq!(aggregate.status, AggregateStatus::Blocked);
        assert_eq!(aggregate.failures[0].pin, "cuda");
        assert!(aggregate
            .failures
            .iter()
            .any(|failure| failure.code == DiagnosticCode::Downgrade));

        let encoded = serde_json::to_value(aggregate).unwrap();
        assert!(encoded.get("mutations").is_none());
    }

    #[test]
    fn evaluate_derives_current_wish_and_downgrade_policy() {
        let wish = WishEvidence {
            package: "gimp".into(),
            current: PackageEvidence {
                package: "gimp".into(),
                availability: Availability::SubstitutableOutput,
                compatibility: Compatibility::Compatible,
                ..PackageEvidence::default()
            },
            candidate: None,
            decision: WishDecision::Unknown,
            reason: None,
        };
        let plan = CachePinPlan::evaluate(
            "cuda",
            "nixpkgs-cuda",
            RevisionEvidence::new(
                Some("current".into()),
                Some("older".into()),
                RevisionRelation::Older,
            ),
            Vec::new(),
            vec![wish],
        );

        assert_eq!(plan.status, PinStatus::Blocked);
        assert_eq!(
            plan.wishes[0].decision,
            WishDecision::CurrentPinPromotionRequired
        );
        assert_eq!(
            plan.revision.policy,
            RevisionPolicy::DowngradeOverrideRequired
        );
    }
}

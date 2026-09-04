//! R4 convergence gate and R7 bounded recovery budget.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const BUDGET_DEFAULT: u64 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifierSnapshot {
    pub verifier_id: String,
    pub evidence_hash: String,
    pub findings: Vec<Finding>,
    pub skipped_tests: u32,
}

impl VerifierSnapshot {
    fn is_attributed(&self) -> bool {
        !self.verifier_id.trim().is_empty() && !self.evidence_hash.trim().is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnergyInput {
    Verifier(VerifierSnapshot),
    SelfReport { claimed_energy: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnergyDecision {
    BaselineAccepted { energy: u64 },
    ProgressAccepted { from: u64, to: u64 },
    SelfReportIgnored,
    RequiresHitlPlateau { energy: u64 },
    RejectedRegression { from: u64, to: u64 },
    RejectedSkippedTests { previous: u32, current: u32 },
    RejectedUnattributedVerifier,
    RejectedUnknownFinding { code: String },
}

#[derive(Debug, Clone)]
pub struct EnergyLedger {
    /// Human-calibrated policy, owned by the control plane rather than the verifier/repairer.
    weights: BTreeMap<String, u32>,
    accepted: Vec<VerifierSnapshot>,
}

impl Default for EnergyLedger {
    fn default() -> Self {
        Self::with_weights(BTreeMap::from([
            ("compile-error".into(), 100),
            ("invariant-violation".into(), 50),
            ("test-failure".into(), 10),
            ("lint-error".into(), 1),
        ]))
    }
}

impl EnergyLedger {
    pub fn with_weights(weights: BTreeMap<String, u32>) -> Self {
        Self {
            weights,
            accepted: Vec::new(),
        }
    }

    pub fn current(&self) -> Option<&VerifierSnapshot> {
        self.accepted.last()
    }

    pub fn history(&self) -> &[VerifierSnapshot] {
        &self.accepted
    }

    /// Only an attributed independent-verifier snapshot can change the ledger.
    /// A repair is accepted only when weighted defect energy strictly decreases.
    pub fn observe(&mut self, input: EnergyInput) -> EnergyDecision {
        let EnergyInput::Verifier(snapshot) = input else {
            return EnergyDecision::SelfReportIgnored;
        };
        if !snapshot.is_attributed() {
            return EnergyDecision::RejectedUnattributedVerifier;
        }
        let Some(energy) = self.score(&snapshot) else {
            let code = snapshot
                .findings
                .iter()
                .find(|finding| !self.weights.contains_key(&finding.code))
                .map(|finding| finding.code.clone())
                .unwrap_or_default();
            return EnergyDecision::RejectedUnknownFinding { code };
        };
        let Some(previous) = self.accepted.last() else {
            self.accepted.push(snapshot);
            return EnergyDecision::BaselineAccepted { energy };
        };
        if snapshot.skipped_tests > previous.skipped_tests {
            return EnergyDecision::RejectedSkippedTests {
                previous: previous.skipped_tests,
                current: snapshot.skipped_tests,
            };
        }
        let from = self
            .score(previous)
            .expect("accepted snapshots have known weights");
        let to = energy;
        if to < from {
            self.accepted.push(snapshot);
            EnergyDecision::ProgressAccepted { from, to }
        } else if to == from {
            EnergyDecision::RequiresHitlPlateau { energy: to }
        } else {
            EnergyDecision::RejectedRegression { from, to }
        }
    }

    fn score(&self, snapshot: &VerifierSnapshot) -> Option<u64> {
        snapshot.findings.iter().try_fold(0_u64, |total, finding| {
            self.weights
                .get(&finding.code)
                .map(|weight| total.saturating_add(u64::from(finding.count) * u64::from(*weight)))
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetDecision {
    Granted { remaining: u64 },
    RecoveryGranted { recovery_remaining: u64 },
    Exhausted,
}

/// Normal work cannot consume the recovery reserve. Once normal budget is exhausted,
/// recovery may use that reserve only to checkpoint and produce the partial-delivery report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryBudget {
    remaining: u64,
    recovery_remaining: u64,
}

impl RecoveryBudget {
    pub fn new(normal: u64, recovery_reserve: u64) -> Self {
        Self {
            remaining: normal,
            recovery_remaining: recovery_reserve,
        }
    }

    pub fn spend(&mut self, cost: u64) -> BudgetDecision {
        if cost <= self.remaining {
            self.remaining -= cost;
            BudgetDecision::Granted {
                remaining: self.remaining,
            }
        } else {
            self.remaining = 0;
            BudgetDecision::Exhausted
        }
    }

    pub fn spend_recovery(&mut self, cost: u64) -> BudgetDecision {
        if self.remaining > 0 || cost > self.recovery_remaining {
            return BudgetDecision::Exhausted;
        }
        self.recovery_remaining -= cost;
        BudgetDecision::RecoveryGranted {
            recovery_remaining: self.recovery_remaining,
        }
    }

    pub fn is_exhausted(&self) -> bool {
        self.remaining == 0
    }

    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(defects: u32, skipped_tests: u32) -> VerifierSnapshot {
        VerifierSnapshot {
            verifier_id: "independent-verifier".into(),
            evidence_hash: format!("sha256:{defects}:{skipped_tests}"),
            findings: vec![Finding {
                code: "test-failure".into(),
                count: defects,
            }],
            skipped_tests,
        }
    }

    #[test]
    fn accepts_only_strictly_decreasing_verifier_energy() {
        let mut ledger = EnergyLedger::default();
        assert_eq!(
            ledger.observe(EnergyInput::Verifier(snapshot(3, 0))),
            EnergyDecision::BaselineAccepted { energy: 30 }
        );
        assert_eq!(
            ledger.observe(EnergyInput::SelfReport { claimed_energy: 0 }),
            EnergyDecision::SelfReportIgnored
        );
        assert_eq!(
            ledger.observe(EnergyInput::Verifier(snapshot(3, 0))),
            EnergyDecision::RequiresHitlPlateau { energy: 30 }
        );
        assert_eq!(
            ledger.observe(EnergyInput::Verifier(snapshot(2, 0))),
            EnergyDecision::ProgressAccepted { from: 30, to: 20 }
        );
    }

    #[test]
    fn increasing_skips_cannot_fake_progress() {
        let mut ledger = EnergyLedger::default();
        ledger.observe(EnergyInput::Verifier(snapshot(3, 0)));
        assert_eq!(
            ledger.observe(EnergyInput::Verifier(snapshot(0, 1))),
            EnergyDecision::RejectedSkippedTests {
                previous: 0,
                current: 1
            }
        );
    }

    #[test]
    fn verifier_cannot_invent_or_reweight_finding_classes() {
        let mut ledger = EnergyLedger::default();
        let mut value = snapshot(1, 0);
        value.findings[0].code = "repairer-selected-cheap-weight".into();
        assert_eq!(
            ledger.observe(EnergyInput::Verifier(value)),
            EnergyDecision::RejectedUnknownFinding {
                code: "repairer-selected-cheap-weight".into()
            }
        );
    }

    #[test]
    fn recovery_reserve_is_available_only_after_normal_exhaustion() {
        let mut budget = RecoveryBudget::new(5, 2);
        assert_eq!(budget.spend_recovery(1), BudgetDecision::Exhausted);
        assert!(matches!(
            budget.spend(5),
            BudgetDecision::Granted { remaining: 0 }
        ));
        assert!(matches!(
            budget.spend_recovery(2),
            BudgetDecision::RecoveryGranted {
                recovery_remaining: 0
            }
        ));
        assert_eq!(budget.spend_recovery(1), BudgetDecision::Exhausted);
    }

    #[test]
    fn over_budget_attempt_enters_explicit_exhausted_state() {
        let mut budget = RecoveryBudget::new(3, 1);
        assert_eq!(budget.spend(4), BudgetDecision::Exhausted);
        assert!(budget.is_exhausted());
        assert!(matches!(
            budget.spend_recovery(1),
            BudgetDecision::RecoveryGranted { .. }
        ));
    }
}

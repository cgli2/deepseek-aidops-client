//! 运行时唯一任务进度真相源；模型计划文本只能提出建议，不能推进验收状态。
use std::collections::BTreeMap;

use crate::execution::TaskContract;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LedgerStatus {
    Pending,
    Active,
    Evidence,
    Verified,
    Blocked,
}

#[derive(Debug, Clone)]
pub struct LedgerItem {
    pub id: String,
    pub description: String,
    pub status: LedgerStatus,
    pub evidence: Vec<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TaskLedger {
    items: BTreeMap<String, LedgerItem>,
}

impl TaskLedger {
    pub fn from_contract(contract: &TaskContract) -> Self {
        Self {
            items: contract
                .acceptance_criteria
                .iter()
                .map(|c| {
                    (
                        c.id.clone(),
                        LedgerItem {
                            id: c.id.clone(),
                            description: c.description.clone(),
                            status: LedgerStatus::Pending,
                            evidence: vec![],
                            reason: None,
                        },
                    )
                })
                .collect(),
        }
    }
    pub fn activate(&mut self, id: &str) -> bool {
        self.items
            .get_mut(id)
            .map(|i| {
                if i.status == LedgerStatus::Pending {
                    i.status = LedgerStatus::Active;
                }
                true
            })
            .unwrap_or(false)
    }
    pub fn add_evidence(&mut self, id: &str, evidence: String) -> bool {
        self.items
            .get_mut(id)
            .map(|i| {
                i.status = LedgerStatus::Evidence;
                i.evidence.push(evidence);
                true
            })
            .unwrap_or(false)
    }
    pub fn verify(&mut self, id: &str) -> bool {
        self.items
            .get_mut(id)
            .map(|i| {
                if !i.evidence.is_empty() {
                    i.status = LedgerStatus::Verified;
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false)
    }
    pub fn block(&mut self, id: &str, reason: String) {
        if let Some(i) = self.items.get_mut(id) {
            i.status = LedgerStatus::Blocked;
            i.reason = Some(reason);
        }
    }
    pub fn next_pending(&self) -> Option<&LedgerItem> {
        self.items.values().find(|i| {
            matches!(
                i.status,
                LedgerStatus::Pending | LedgerStatus::Active | LedgerStatus::Evidence
            )
        })
    }
    pub fn all_verified(&self) -> bool {
        self.items
            .values()
            .all(|i| i.status == LedgerStatus::Verified)
    }
    pub fn verified_count(&self) -> usize {
        self.items
            .values()
            .filter(|i| i.status == LedgerStatus::Verified)
            .count()
    }
    pub fn blocked_count(&self) -> usize {
        self.items
            .values()
            .filter(|i| i.status == LedgerStatus::Blocked)
            .count()
    }
    pub fn current_item(&self) -> Option<&LedgerItem> {
        self.next_pending()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::TaskContract;
    #[test]
    fn needs_evidence_before_verify() {
        let mut l = TaskLedger::from_contract(&TaskContract::from_input("- a\n- b"));
        assert!(!l.verify("item-1"));
        l.activate("item-1");
        l.add_evidence("item-1", "cargo test ok".into());
        assert!(l.verify("item-1"));
        assert!(!l.all_verified());
    }
}

//! Audit result types, shared by all auditor implementations.

use std::fmt;

use crate::domain::ResolvedSkill;

/// Identifier of an auditor implementation (e.g. `noop`, `static`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AuditorId(pub &'static str);

impl fmt::Display for AuditorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

/// Severity of a finding. Ordering matters: the worst severity wins when
/// aggregating a report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub enum Severity {
    #[default]
    Pass,
    Warn,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub severity: Severity,
    pub message: String,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditReport {
    pub findings: Vec<Finding>,
}

impl AuditReport {
    /// Worst severity across findings; an empty report passes.
    pub fn worst(&self) -> Severity {
        self.findings
            .iter()
            .map(|f| f.severity)
            .max()
            .unwrap_or(Severity::Pass)
    }
}

/// Output of the Audit stage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditedSkill {
    pub skill: ResolvedSkill,
    pub verdict: Severity,
    pub findings: Vec<Finding>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_report_passes() {
        assert_eq!(AuditReport::default().worst(), Severity::Pass);
    }

    #[test]
    fn worst_severity_wins() {
        let report = AuditReport {
            findings: vec![
                Finding {
                    severity: Severity::Warn,
                    message: "w".into(),
                    location: None,
                },
                Finding {
                    severity: Severity::Block,
                    message: "b".into(),
                    location: None,
                },
                Finding {
                    severity: Severity::Pass,
                    message: "p".into(),
                    location: None,
                },
            ],
        };
        assert_eq!(report.worst(), Severity::Block);
    }
}

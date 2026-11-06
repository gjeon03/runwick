//! Roles (작업 방식): reusable instructions, a preferred tool and an optional checklist that a
//! session can be given when work is delegated. A role is not a team member or a session.

use crate::Ts;
use crate::api::projects::ById;
use crate::api::sessions::Tool;
use crate::method;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Topic for the role list.
pub const TOPIC: &str = "roles";

pub type RoleId = Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Role {
    pub id: RoleId,
    pub name: String,
    pub instructions: String,
    #[serde(default)]
    pub preferred_tool: Option<Tool>,
    /// Optional checklist, one step per entry.
    #[serde(default)]
    pub procedure: Vec<String>,
    pub created_at: Ts,
    pub updated_at: Ts,
}

impl Role {
    /// The text a delegated session receives for this role (also shown as the preview).
    pub fn brief(&self) -> String {
        let mut out = format!("역할: {}\n", self.name);
        if let Some(t) = self.preferred_tool {
            out.push_str(&format!("선호 도구: {}\n", t.label()));
        }
        let instructions = self.instructions.trim();
        if !instructions.is_empty() {
            out.push('\n');
            out.push_str(instructions);
            out.push('\n');
        }
        let steps: Vec<&str> = self.procedure.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        if !steps.is_empty() {
            out.push_str("\n절차:\n");
            for (i, s) in steps.iter().enumerate() {
                out.push_str(&format!("{}. {s}\n", i + 1));
            }
        }
        out
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RoleSave {
    /// Existing role to update; a new role when `None`.
    #[serde(default)]
    pub id: Option<RoleId>,
    pub name: String,
    #[serde(default)]
    pub instructions: String,
    #[serde(default)]
    pub preferred_tool: Option<Tool>,
    #[serde(default)]
    pub procedure: Vec<String>,
}

method!(RolesList, "roles.list", () => Vec<Role>, [TOPIC]);
method!(RolesGet, "roles.get", ById => Role, [TOPIC]);
method!(RolesSave, "roles.save", RoleSave => Role);
method!(RolesDuplicate, "roles.duplicate", ById => Role);
method!(RolesDelete, "roles.delete", ById => ());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brief_lists_instructions_and_numbered_steps() {
        let r = Role {
            id: Uuid::nil(),
            name: "회귀 검토".into(),
            instructions: "  변경된 부분의 회귀를 찾는다  ".into(),
            preferred_tool: Some(Tool::Codex),
            procedure: vec!["테스트 실행".into(), " ".into(), "결과 보고".into()],
            created_at: crate::now(),
            updated_at: crate::now(),
        };
        let b = r.brief();
        assert!(b.starts_with("역할: 회귀 검토\n선호 도구: Codex\n"), "{b}");
        assert!(b.contains("\n변경된 부분의 회귀를 찾는다\n"), "{b}");
        assert!(b.ends_with("절차:\n1. 테스트 실행\n2. 결과 보고\n"), "{b}");
    }
}

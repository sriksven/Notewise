//! What first-run setup still needs, and which of it applies to this build.
//!
//! Kept apart from the route table because it is policy, not transport: whether a capability
//! blocks the user is a decision worth testing without constructing a request.

use serde::Serialize;

/// The key under which completion is recorded in `app_settings`.
pub(crate) const COMPLETED_KEY: &str = "onboarding_completed_at";

#[derive(Debug, Serialize)]
pub(crate) struct SetupReadiness {
    /// RFC 3339, or `None` while setup has never been finished.
    pub completed_at: Option<String>,
    pub steps: Steps,
}

#[derive(Debug, Serialize)]
pub(crate) struct Steps {
    pub model: StepReadiness,
    pub backend: StepReadiness,
    pub permissions: PermissionsReadiness,
}

#[derive(Debug, Serialize)]
pub(crate) struct StepReadiness {
    pub satisfied: bool,
    pub required: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct PermissionReadiness {
    /// `not_requested` | `granted` | `denied` | `unavailable`.
    pub status: String,
    pub required: bool,
    /// Why it is unavailable, when it is. Shown to the user verbatim.
    pub detail: Option<String>,
}

#[derive(Debug, Serialize)]
pub(crate) struct PermissionsReadiness {
    pub satisfied: bool,
    pub required: bool,
    pub microphone: PermissionReadiness,
    pub system_audio: PermissionReadiness,
}

impl PermissionsReadiness {
    /// Combine per-capability answers into one gate.
    ///
    /// Only `granted` satisfies a required capability. Anything `unavailable` is excluded
    /// from the gate entirely — there is no action a user could take, so blocking on it would
    /// trap them in the wizard. That is the whole of the "required only when available" rule.
    pub fn from_parts(microphone: PermissionReadiness, system_audio: PermissionReadiness) -> Self {
        let satisfied = [&microphone, &system_audio]
            .into_iter()
            .filter(|p| p.required)
            .all(|p| p.status == "granted");

        let required = microphone.required || system_audio.required;

        Self {
            satisfied,
            required,
            microphone,
            system_audio,
        }
    }
}

impl SetupReadiness {
    /// The names of required steps that are not satisfied, in wizard order.
    ///
    /// Returned rather than a bare bool so a rejected completion can say which step is
    /// missing instead of "setup incomplete".
    pub fn unsatisfied(&self) -> Vec<&'static str> {
        let mut names = Vec::new();

        if self.steps.model.required && !self.steps.model.satisfied {
            names.push("model");
        }
        if self.steps.backend.required && !self.steps.backend.satisfied {
            names.push("backend");
        }
        if self.steps.permissions.required && !self.steps.permissions.satisfied {
            names.push("permissions");
        }

        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission(status: &str, required: bool) -> PermissionReadiness {
        PermissionReadiness {
            status: status.into(),
            required,
            detail: None,
        }
    }

    #[test]
    fn an_unavailable_capability_does_not_block() {
        let readiness = PermissionsReadiness::from_parts(
            permission("granted", true),
            PermissionReadiness {
                status: "unavailable".into(),
                required: false,
                detail: Some("no signed bundle".into()),
            },
        );

        assert!(
            readiness.satisfied,
            "a capability nobody can grant must not gate the user"
        );
    }

    #[test]
    fn a_denied_required_capability_blocks() {
        let readiness = PermissionsReadiness::from_parts(
            permission("denied", true),
            permission("unavailable", false),
        );

        assert!(!readiness.satisfied);
    }

    #[test]
    fn an_unrequested_required_capability_blocks() {
        let readiness = PermissionsReadiness::from_parts(
            permission("not_requested", true),
            permission("unavailable", false),
        );

        assert!(!readiness.satisfied);
    }

    #[test]
    fn unsatisfied_steps_are_named_so_a_refusal_can_say_which() {
        let setup = SetupReadiness {
            completed_at: None,
            steps: Steps {
                model: StepReadiness {
                    satisfied: false,
                    required: true,
                },
                backend: StepReadiness {
                    satisfied: true,
                    required: true,
                },
                permissions: PermissionsReadiness::from_parts(
                    permission("denied", true),
                    permission("unavailable", false),
                ),
            },
        };

        assert_eq!(setup.unsatisfied(), vec!["model", "permissions"]);
    }

    #[test]
    fn everything_satisfied_leaves_nothing_unsatisfied() {
        let setup = SetupReadiness {
            completed_at: None,
            steps: Steps {
                model: StepReadiness {
                    satisfied: true,
                    required: true,
                },
                backend: StepReadiness {
                    satisfied: true,
                    required: true,
                },
                permissions: PermissionsReadiness::from_parts(
                    permission("granted", true),
                    permission("unavailable", false),
                ),
            },
        };

        assert!(setup.unsatisfied().is_empty());
    }
}

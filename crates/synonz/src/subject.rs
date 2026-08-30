//! The interaction subject: the owner of memory.
//!
//! A subject is a first-class entity identifying one side of an
//! interaction (a user today; agents themselves when multi-agent
//! orchestration arrives in S3). Identity is the `(SubjectType, id)`
//! pair — `user-42` and `agent-42` are different subjects.

use serde::{Deserialize, Serialize};

/// The kind of interaction participant.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectType {
    /// A user subject.
    User,
    /// An agent subject. The variant exists from day one (predictable
    /// extension); the interaction semantics of agent memories arrive
    /// with multi-agent orchestration (S3).
    Agent,
}

/// An interaction subject: the owner of memory.
///
/// Cheap to clone and compare; equality is identity equality (type + id).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Subject {
    subject_type: SubjectType,
    id: String,
}

impl Subject {
    /// Resolves a subject by its identity (the `of` family: restore by
    /// identity, never create a fresh one implicitly).
    pub fn of(subject_type: SubjectType, id: impl Into<String>) -> Self {
        Self {
            subject_type,
            id: id.into(),
        }
    }

    /// The subject's type.
    pub fn subject_type(&self) -> SubjectType {
        self.subject_type
    }

    /// The subject's id.
    pub fn id(&self) -> &str {
        &self.id
    }
}

impl std::fmt::Display for Subject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}:{}", self.subject_type, self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_type_and_id() {
        let user = Subject::of(SubjectType::User, "42");
        let agent = Subject::of(SubjectType::Agent, "42");
        assert_ne!(user, agent, "same id, different type => different subject");
        assert_eq!(user, Subject::of(SubjectType::User, "42"));
    }

    #[test]
    fn accessors() {
        let subject = Subject::of(SubjectType::User, "user-42");
        assert_eq!(subject.subject_type(), SubjectType::User);
        assert_eq!(subject.id(), "user-42");
    }
}

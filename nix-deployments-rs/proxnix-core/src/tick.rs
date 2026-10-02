#[pure_only]
use crate::guest::DurationMs;
#[pure_only]
use crate::tags::CommitHash;
use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Moment(pub u64);

#[pure_only]
impl Moment {
    #[must_use]
    pub fn after(self, span: DurationMs) -> Moment {
        Moment(self.0.saturating_add(span.0))
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Push {
    commit: CommitHash,
}

#[pure_only]
impl Push {
    #[must_use]
    pub fn new(commit: CommitHash) -> Push {
        Push { commit }
    }

    #[must_use]
    pub fn commit(&self) -> &CommitHash {
        &self.commit
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick {
    Push(Push),
    Periodic,
}

#[pure_only]
impl Tick {
    #[must_use]
    pub fn push(&self) -> Option<&Push> {
        match self {
            Tick::Push(push) => Some(push),
            Tick::Periodic => None,
        }
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacing {
    pub address: DurationMs,
    pub port: DurationMs,
    pub guest: DurationMs,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_moment_never_wraps() {
        assert_eq!(Moment(10).after(DurationMs(5)), Moment(15));
        assert_eq!(Moment(u64::MAX).after(DurationMs(5)), Moment(u64::MAX));
    }

    #[test]
    fn only_a_push_carries_a_push() {
        let push = Push::new("66d0ba6b605de2703e0fb7bbf58b922d5b36597e".parse().unwrap());
        assert_eq!(Tick::Push(push.clone()).push(), Some(&push));
        assert_eq!(Tick::Periodic.push(), None);
    }
}

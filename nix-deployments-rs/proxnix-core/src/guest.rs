use proxnix_pure::pure_only;

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuestKind {
    Qemu,
    Lxc,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestStatus {
    Running,
    Stopped,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryMb(pub u32);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiskGib(pub u32);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Cores(pub u16);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sockets(pub u8);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Port(pub u16);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DurationMs(pub u64);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Attempt(pub u32);

#[pure_only]
impl Attempt {
    #[must_use]
    pub fn next(self) -> Attempt {
        Attempt(self.0.saturating_add(1))
    }
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resources {
    pub memory: MemoryMb,
    pub disk: DiskGib,
    pub cores: Cores,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Privilege {
    Privileged,
    Unprivileged,
}

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MountMode {
    ReadOnly,
    ReadWrite,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathPart(String);

#[pure_only]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathFault {
    Relative,
    EmptyPart,
    Climbs,
    InStore,
}

#[pure_only]
impl TryFrom<&str> for PathPart {
    type Error = PathFault;

    fn try_from(text: &str) -> Result<PathPart, PathFault> {
        match text {
            "" | "." => Err(PathFault::EmptyPart),
            ".." => Err(PathFault::Climbs),
            part if part.contains('/') => Err(PathFault::EmptyPart),
            part => Ok(PathPart(String::from(part))),
        }
    }
}

#[pure_only]
impl AsRef<str> for PathPart {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostPath(Vec<PathPart>);

#[pure_only]
impl TryFrom<&str> for HostPath {
    type Error = PathFault;

    fn try_from(text: &str) -> Result<HostPath, PathFault> {
        let relative = text.strip_prefix('/').ok_or(PathFault::Relative)?;
        let parts: Vec<PathPart> = relative
            .split('/')
            .filter(|part| !part.is_empty())
            .map(PathPart::try_from)
            .collect::<Result<_, _>>()?;
        match parts.as_slice() {
            [nix, store, ..] if nix.0 == "nix" && store.0 == "store" => Err(PathFault::InStore),
            _ => Ok(HostPath(parts)),
        }
    }
}

#[pure_only]
impl HostPath {
    #[must_use]
    pub fn parts(&self) -> &[PathPart] {
        &self.0
    }

    pub(crate) fn within(
        dataset: &crate::layout::Dataset,
        rest: &[crate::layout::Segment],
    ) -> HostPath {
        HostPath(
            dataset
                .segments()
                .iter()
                .chain(rest)
                .map(|segment| PathPart(String::from(segment.as_ref())))
                .collect(),
        )
    }
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GuestPath(pub String);

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mount {
    pub host: HostPath,
    pub guest: GuestPath,
    pub mode: MountMode,
}

#[pure_only]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindFacts {
    Qemu {
        sockets: Sockets,
    },
    Lxc {
        privilege: Privilege,
        mounts: Vec<Mount>,
    },
}

#[pure_only]
impl KindFacts {
    #[must_use]
    pub fn kind(&self) -> GuestKind {
        match self {
            KindFacts::Qemu { .. } => GuestKind::Qemu,
            KindFacts::Lxc { .. } => GuestKind::Lxc,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempts_count_up_without_overflowing() {
        assert_eq!(Attempt(0).next(), Attempt(1));
        assert_eq!(Attempt(u32::MAX).next(), Attempt(u32::MAX));
    }

    #[test]
    fn facts_know_their_kind() {
        assert_eq!(
            KindFacts::Qemu {
                sockets: Sockets(1)
            }
            .kind(),
            GuestKind::Qemu
        );
        assert_eq!(
            KindFacts::Lxc {
                privilege: Privilege::Unprivileged,
                mounts: vec![]
            }
            .kind(),
            GuestKind::Lxc
        );
    }
}

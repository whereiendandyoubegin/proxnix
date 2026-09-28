use crate::engine::{Realisation, fault};
use crate::nix::{NixFault, run};
use crate::types::{AppError, Result};
use proxnix_core::{Detail, Key, RootHolder, StoreEffect, StoreEvent, StorePath, Toplevel, Vmid};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::{info, warn};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Flake {
    pub repo: PathBuf,
    pub dir: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NixStore {
    root: PathBuf,
    cache: String,
    flake: Flake,
    timeout: Duration,
}

pub(crate) fn store_text(path: &StorePath) -> String {
    format!("/nix/store/{}-{}", path.hash().as_ref(), path.name().as_ref())
}

fn holder_name(holder: &RootHolder) -> String {
    match holder {
        RootHolder::Guest(id) => format!("guest-{}", id.get()),
        RootHolder::Recent(key) => format!("recent-{}-{}", key.rev.as_ref(), key.job.0),
    }
}

fn holder_of(name: &str) -> Option<RootHolder> {
    match name.split_once('-') {
        Some(("guest", id)) => id.parse().ok().map(|id| RootHolder::Guest(Vmid::new(id))),
        Some(("recent", rest)) => {
            let (rev, job) = rest.split_once('-')?;
            Some(RootHolder::Recent(Key { job: proxnix_core::ImageType(String::from(job)), rev: rev.parse().ok()? }))
        }
        _ => None,
    }
}

impl NixStore {
    pub fn new(root: PathBuf, cache: String, flake: Flake, timeout: Duration) -> NixStore {
        NixStore { root, cache, flake, timeout }
    }

    fn uri(&self) -> String {
        format!("local?root={}", self.root.display())
    }

    fn roots(&self) -> PathBuf {
        self.root.join("nix/var/nix/gcroots/proxnix")
    }

    fn copy_args(&self, toplevel: &Toplevel) -> Vec<String> {
        vec![
            String::from("copy"),
            String::from("--from"),
            self.cache.clone(),
            String::from("--to"),
            self.uri(),
            store_text(toplevel.path()),
        ]
    }

    fn build_args(&self, key: &Key) -> Vec<String> {
        let dir = self.flake.dir.as_ref().map_or(String::new(), |dir| format!("&dir={dir}"));
        vec![
            String::from("build"),
            String::from("--store"),
            self.uri(),
            String::from("--extra-substituters"),
            self.cache.clone(),
            String::from("--no-link"),
            String::from("--print-out-paths"),
            format!(
                "git+file://{}?rev={}{dir}#nixosConfigurations.{}.config.system.build.toplevel",
                self.flake.repo.display(),
                key.rev.as_ref(),
                key.job.0
            ),
        ]
    }

    fn collect_args(&self) -> Vec<String> {
        vec![String::from("--store"), self.uri(), String::from("--gc")]
    }

    fn tool(&self, program: &str, label: &str, args: &[String]) -> std::result::Result<String, NixFault> {
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        run(program, Path::new("/"), label, &args, self.timeout)
    }

    fn copy(&self, key: &Key, toplevel: &Toplevel) -> std::result::Result<(), Detail> {
        info!("copying {} into the store for {}", store_text(toplevel.path()), key.job.0);
        self.tool("nix", &key.job.0, &self.copy_args(toplevel)).map(|_| ()).map_err(|fault| Detail(format!("{fault:?}")))
    }

    fn build(&self, key: &Key) -> std::result::Result<Toplevel, proxnix_core::BuildFault> {
        info!("building {} at {} into the store", key.job.0, key.rev.as_ref());
        let built = self.tool("nix", &key.job.0, &self.build_args(key)).map_err(|nix| fault(nix, Realisation::Build))?;
        let line = built.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default();
        line.parse::<StorePath>().map(Toplevel::from).map_err(proxnix_core::BuildFault::Output)
    }

    pub(crate) fn root(&self, holder: &RootHolder, toplevel: &Toplevel) -> Result<()> {
        let dir = self.roots();
        std::fs::create_dir_all(&dir)?;
        let link = dir.join(holder_name(holder));
        let staged = dir.join(format!(".{}.{}", holder_name(holder), std::process::id()));
        std::os::unix::fs::symlink(store_text(toplevel.path()), &staged)?;
        std::fs::rename(&staged, &link).map_err(AppError::from)
    }

    pub(crate) fn unroot(&self, holder: &RootHolder) -> Result<()> {
        match std::fs::remove_file(self.roots().join(holder_name(holder))) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(AppError::from(error)),
            _ => Ok(()),
        }
    }

    fn collect(&self) -> Result<()> {
        self.tool("nix-store", "gc", &self.collect_args()).map(|_| ()).map_err(AppError::from)
    }

    pub fn present(&self) -> Result<BTreeSet<Toplevel>> {
        match std::fs::read_dir(self.root.join("nix/store")) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeSet::new()),
            Err(error) => Err(AppError::from(error)),
            Ok(entries) => Ok(entries
                .filter_map(std::result::Result::ok)
                .filter_map(|entry| format!("/nix/store/{}", entry.file_name().to_string_lossy()).parse::<StorePath>().ok())
                .map(Toplevel::from)
                .collect()),
        }
    }

    pub fn rooted(&self) -> Result<BTreeMap<RootHolder, Toplevel>> {
        match std::fs::read_dir(self.roots()) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(AppError::from(error)),
            Ok(entries) => Ok(entries
                .filter_map(std::result::Result::ok)
                .filter_map(|entry| {
                    let holder = holder_of(&entry.file_name().to_string_lossy())?;
                    let target = std::fs::read_link(entry.path()).ok()?;
                    let path = target.to_str()?.parse::<StorePath>().ok()?;
                    Some((holder, Toplevel::from(path)))
                })
                .collect()),
        }
    }

    fn quietly(&self, what: &str, done: Result<()>) {
        if let Err(error) = done {
            warn!("store {what} failed: {error}");
        }
    }

    pub fn apply(&self, effects: &[StoreEffect]) -> Vec<StoreEvent> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                StoreEffect::Copy { key, toplevel } => Some(StoreEvent::Copied { key: key.clone(), outcome: self.copy(key, toplevel) }),
                StoreEffect::Build { key } => Some(StoreEvent::Built { key: key.clone(), outcome: self.build(key) }),
                StoreEffect::Root { holder, toplevel } => {
                    self.quietly("root", self.root(holder, toplevel));
                    None
                }
                StoreEffect::Unroot(holder) => {
                    self.quietly("unroot", self.unroot(holder));
                    None
                }
                StoreEffect::Collect => {
                    self.quietly("collection", self.collect());
                    None
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxnix_core::ImageType;

    const REV: &str = "b44ce58f9c9d8565bbdd2990f54c3e91b2c8082e";
    const TOPLEVEL: &str = "/nix/store/78s0iadvjz6s48aqvx4rw78lwrzkjzlw-nixos-system-forgejo-26.11";

    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("proxnix-store-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn store(root: PathBuf, dir: Option<&str>) -> NixStore {
        NixStore::new(
            root,
            String::from("file:///ZFS/hydra-cache"),
            Flake { repo: PathBuf::from("/tmp/proxnix/repos/nixology"), dir: dir.map(String::from) },
            Duration::from_secs(1),
        )
    }

    fn key(job: &str) -> Key {
        Key { job: ImageType(String::from(job)), rev: REV.parse().unwrap() }
    }

    fn toplevel(text: &str) -> Toplevel {
        Toplevel::from(text.parse::<StorePath>().unwrap())
    }

    #[test]
    fn a_copy_pulls_one_toplevel_from_the_hydra_cache_into_the_chroot_store() {
        assert_eq!(
            store(PathBuf::from("/ZFS/proxnix/store"), None).copy_args(&toplevel(TOPLEVEL)).join(" "),
            format!("copy --from file:///ZFS/hydra-cache --to local?root=/ZFS/proxnix/store {TOPLEVEL}")
        );
    }

    #[test]
    fn a_build_evaluates_the_pushed_commit_not_whatever_the_checkout_holds() {
        assert_eq!(
            store(PathBuf::from("/ZFS/proxnix/store"), None).build_args(&key("build-lxc-forgejo")).join(" "),
            format!(
                "build --store local?root=/ZFS/proxnix/store --extra-substituters file:///ZFS/hydra-cache --no-link --print-out-paths \
                 git+file:///tmp/proxnix/repos/nixology?rev={REV}#nixosConfigurations.build-lxc-forgejo.config.system.build.toplevel"
            )
        );
        assert!(store(PathBuf::from("/s"), Some("infra")).build_args(&key("build-lxc")).last().unwrap().contains(&format!("?rev={REV}&dir=infra#")));
        assert_eq!(store(PathBuf::from("/s"), None).collect_args().join(" "), "--store local?root=/s --gc");
    }

    #[test]
    fn root_names_survive_a_round_trip_even_with_dashes_in_the_job() {
        for holder in [RootHolder::Guest(Vmid::new(844)), RootHolder::Recent(key("build-lxc-neon-safekeeper-1"))] {
            assert_eq!(holder_of(&holder_name(&holder)), Some(holder));
        }
        assert_eq!(holder_of("somebody-elses-root"), None);
        assert_eq!(holder_of(".guest-844.1234"), None);
    }

    #[test]
    fn roots_are_written_read_back_replaced_and_removed() {
        let nix = store(scratch("roots"), None);
        let guest = RootHolder::Guest(Vmid::new(844));
        let newer = "/nix/store/i3d00236fdkfw1v9cmasajkjhzl8zi5j-nixos-system-forgejo-26.11";
        nix.root(&guest, &toplevel(TOPLEVEL)).unwrap();
        nix.root(&RootHolder::Recent(key("build-lxc-forgejo")), &toplevel(newer)).unwrap();
        assert_eq!(nix.rooted().unwrap().get(&guest), Some(&toplevel(TOPLEVEL)));
        nix.root(&guest, &toplevel(newer)).unwrap();
        assert_eq!(nix.rooted().unwrap().get(&guest), Some(&toplevel(newer)));
        nix.unroot(&guest).unwrap();
        nix.unroot(&guest).unwrap();
        assert_eq!(nix.rooted().unwrap().keys().collect::<Vec<_>>(), vec![&RootHolder::Recent(key("build-lxc-forgejo"))]);
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git").current_dir(repo).args(args).output().unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    }

    #[test]
    #[ignore = "runs real nix builds into a scratch chroot store; needs nix and network"]
    fn a_real_build_lands_in_the_chroot_store_is_rooted_and_survives_collection() {
        let root = scratch("real");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(
            repo.join("flake.nix"),
            r#"{ inputs.nixpkgs.url = "nixpkgs"; outputs = { nixpkgs, ... }: { nixosConfigurations.tiny.config.system.build.toplevel = nixpkgs.legacyPackages.x86_64-linux.runCommand "nixos-system-tiny" {} "echo tiny > $out"; }; }"#,
        )
        .unwrap();
        git(&repo, &["init", "-q"]);
        git(&repo, &["add", "flake.nix"]);
        git(&repo, &["-c", "user.name=proxnix", "-c", "user.email=proxnix@localhost", "commit", "-qm", "tiny"]);
        let rev = git(&repo, &["rev-parse", "HEAD"]);
        let nix = NixStore::new(root.join("store"), String::from("https://cache.nixos.org"), Flake { repo, dir: None }, Duration::from_secs(600));
        let wanted = Key { job: ImageType(String::from("tiny")), rev: rev.parse().unwrap() };
        let built = nix.build(&wanted).unwrap();
        assert!(nix.present().unwrap().contains(&built));
        nix.root(&RootHolder::Recent(wanted.clone()), &built).unwrap();
        nix.collect().unwrap();
        assert!(nix.present().unwrap().contains(&built), "a rooted toplevel must survive collection");
        nix.unroot(&RootHolder::Recent(wanted)).unwrap();
        nix.collect().unwrap();
        assert!(!nix.present().unwrap().contains(&built), "an unrooted toplevel must be collected");
        let _ = std::process::Command::new("chmod").args(["-R", "u+w"]).arg(&root).status();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_store_holds_nothing_and_a_filled_one_lists_its_paths() {
        let root = scratch("present");
        let nix = store(root.clone(), None);
        assert!(nix.present().unwrap().is_empty());
        assert!(nix.rooted().unwrap().is_empty());
        std::fs::create_dir_all(root.join("nix/store/78s0iadvjz6s48aqvx4rw78lwrzkjzlw-nixos-system-forgejo-26.11")).unwrap();
        std::fs::create_dir_all(root.join("nix/store/.links")).unwrap();
        assert_eq!(nix.present().unwrap(), [toplevel(TOPLEVEL)].into());
    }
}

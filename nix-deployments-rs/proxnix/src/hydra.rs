use crate::types::{AppError, Result};
use proxnix_core::{CommitHash, HydraBuild, Key, StorePath, Toplevel};
use std::collections::BTreeMap;
use tokio::runtime::Handle;

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct HydraConfig {
    pub url: String,
    pub project: String,
    pub jobset: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Deserialize)]
pub struct EvalId(u64);

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Eval {
    id: EvalId,
    #[serde(default)]
    flake: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Evals {
    evals: Vec<Eval>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct Output {
    path: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Build {
    job: String,
    finished: u8,
    buildstatus: Option<i64>,
    #[serde(default)]
    buildoutputs: BTreeMap<String, Output>,
}

fn rev_of(eval: &Eval) -> Option<CommitHash> {
    eval.flake
        .as_deref()?
        .split(['?', '&'])
        .find_map(|pair| pair.strip_prefix("rev="))
        .and_then(|rev| rev.parse().ok())
}

fn outcome(build: &Build) -> HydraBuild {
    match (build.finished, build.buildstatus) {
        (0, _) => HydraBuild::Queued,
        (_, Some(0)) => build
            .buildoutputs
            .get("out")
            .and_then(|out| out.path.parse::<StorePath>().ok())
            .map_or(HydraBuild::Failed, |path| {
                HydraBuild::Succeeded(Toplevel::from(path))
            }),
        _ => HydraBuild::Failed,
    }
}

pub fn view(
    evals: &[Eval],
    builds: &BTreeMap<EvalId, Vec<Build>>,
    wanted: &[Key],
) -> BTreeMap<Key, HydraBuild> {
    wanted
        .iter()
        .map(|key| {
            let newest = evals
                .iter()
                .filter(|eval| rev_of(eval).as_ref() == Some(&key.rev))
                .max_by_key(|eval| eval.id);
            let seen = match newest.and_then(|eval| builds.get(&eval.id)) {
                None => HydraBuild::Unevaluated,
                Some(builds) => builds
                    .iter()
                    .find(|build| build.job == key.job.0)
                    .map_or(HydraBuild::Absent, outcome),
            };
            (key.clone(), seen)
        })
        .collect()
}

pub struct Hydra {
    http: reqwest::Client,
    config: HydraConfig,
    runtime: Handle,
}

impl Hydra {
    pub fn new(config: HydraConfig, runtime: Handle) -> Hydra {
        Hydra {
            http: reqwest::Client::new(),
            config,
            runtime,
        }
    }

    fn get<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.config.url.trim_end_matches('/'));
        let body = self
            .runtime
            .block_on(async {
                self.http
                    .get(&url)
                    .header("Accept", "application/json")
                    .send()
                    .await?
                    .error_for_status()?
                    .text()
                    .await
            })
            .map_err(|e| AppError::CmdError(format!("hydra {url}: {e}")))?;
        serde_json::from_str(&body).map_err(|e| {
            AppError::CmdError(format!(
                "hydra {url} answered with something unexpected: {e}"
            ))
        })
    }

    pub fn evals(&self) -> Result<Vec<Eval>> {
        self.get::<Evals>(&format!(
            "/jobset/{}/{}/evals",
            self.config.project, self.config.jobset
        ))
        .map(|page| page.evals)
    }

    pub fn builds(&self, eval: EvalId) -> Result<Vec<Build>> {
        self.get(&format!("/eval/{}/builds", eval.0))
    }

    pub fn status(&self, wanted: &[Key]) -> Result<BTreeMap<Key, HydraBuild>> {
        let evals = self.evals()?;
        let relevant: Vec<EvalId> = evals
            .iter()
            .filter(|eval| rev_of(eval).is_some_and(|rev| wanted.iter().any(|key| key.rev == rev)))
            .map(|eval| eval.id)
            .collect();
        let builds = relevant
            .into_iter()
            .map(|id| self.builds(id).map(|builds| (id, builds)))
            .collect::<Result<BTreeMap<_, _>>>()?;
        Ok(view(&evals, &builds, wanted))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proxnix_core::ImageType;

    fn fixture<T: serde::de::DeserializeOwned>(name: &str) -> T {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures/hydra")
            .join(name);
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap())
            .unwrap_or_else(|e| panic!("{name} does not decode: {e}"))
    }

    fn captured() -> (Vec<Eval>, BTreeMap<EvalId, Vec<Build>>) {
        let evals = fixture::<Evals>("evals.json").evals;
        let builds = [24, 64, 68]
            .into_iter()
            .map(|id| (EvalId(id), fixture(&format!("eval-{id}-builds.json"))))
            .collect();
        (evals, builds)
    }

    fn key(job: &str, rev: &str) -> Key {
        Key {
            job: ImageType(String::from(job)),
            rev: rev.parse().unwrap(),
        }
    }

    const MAIN_7450593: &str = "7450593521c6669dc9cb5ceeea74f8ebb3232892";
    const MAIN_B44CE58: &str = "b44ce58f9c9d8565bbdd2990f54c3e91b2c8082e";
    const MAIN_8DF1621: &str = "8df162176f7f141e2fbb5e7713f0052e69ccc5dc";

    #[test]
    fn an_evaluation_is_matched_to_its_commit_through_the_locked_flake_url() {
        let (evals, _) = captured();
        let revs: Vec<(u64, String)> = evals
            .iter()
            .filter_map(|eval| rev_of(eval).map(|rev| (eval.id.0, String::from(rev.as_ref()))))
            .collect();
        assert!(revs.contains(&(68, String::from(MAIN_B44CE58))));
        assert!(revs.contains(&(64, String::from(MAIN_7450593))));
        assert_eq!(revs.len(), evals.len(), "every flake eval carries its rev");
    }

    #[test]
    fn captured_hydra_answers_become_build_states() {
        let (evals, builds) = captured();
        let wanted = [
            key("build-lxc-neon-safekeeper-3", MAIN_7450593),
            key("build-lxc-neon-pageserver", MAIN_7450593),
            key("build-lxc-hydra", MAIN_B44CE58),
            key("build-qcow2-website", MAIN_8DF1621),
            key("build-lxc-neon-broker", MAIN_8DF1621),
            key(
                "build-lxc-hydra",
                "0000000000000000000000000000000000000000",
            ),
        ];
        let seen = view(&evals, &builds, &wanted);
        let safekeeper: StorePath = "/nix/store/0w8qq8m1kixrps0m2mlvpr4ziwgk5lc6-tarball"
            .parse()
            .unwrap();
        assert_eq!(
            seen[&wanted[0]],
            HydraBuild::Succeeded(Toplevel::from(safekeeper))
        );
        assert_eq!(seen[&wanted[1]], HydraBuild::Failed, "dependency failed");
        assert_eq!(seen[&wanted[2]], HydraBuild::Queued);
        assert_eq!(seen[&wanted[3]], HydraBuild::Failed, "unsupported system");
        assert_eq!(
            seen[&wanted[4]],
            HydraBuild::Absent,
            "neon did not exist at that commit"
        );
        assert_eq!(seen[&wanted[5]], HydraBuild::Unevaluated);
    }

    #[test]
    fn a_rev_whose_builds_were_not_fetched_counts_as_unevaluated() {
        let (evals, _) = captured();
        let wanted = [key("build-lxc-hydra", MAIN_B44CE58)];
        assert_eq!(
            view(&evals, &BTreeMap::new(), &wanted)[&wanted[0]],
            HydraBuild::Unevaluated
        );
    }
}

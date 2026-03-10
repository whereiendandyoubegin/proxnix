use crate::pct::{copy_to_template_storage, pct_create, pct_start};
use crate::qm::{qm_create, qm_importdisk, qm_resize, qm_set_agent, qm_set_disk, qm_start};
use crate::types::{AppError, ContainerConfig, Result, VMConfig};
use tracing::info;

fn nix_store_hash(store_path: &str) -> Option<&str> {
    store_path
        .strip_prefix("/nix/store/")
        .and_then(|s| s.split('-').next())
}

pub trait Materialise {
    fn nix_build_attr(&self) -> &str;
    fn provision(&self, artifact_path: &str, commit_hash: &str) -> Result<()>;
}

impl Materialise for VMConfig {
    fn nix_build_attr(&self) -> &str {
        "config.system.build.qcow2"
    }
    fn provision(&self, artifact_path: &str, commit_hash: &str) -> Result<()> {
        let nix_hash = nix_store_hash(artifact_path).ok_or_else(|| {
            AppError::CmdError(format!("could not extract nix hash from path {}", artifact_path))
        })?;
        let qcow2_path = format!("{}/nixos.qcow2", artifact_path);
        info!("Provisioning VM {} (id: {})", self.name, self.vm_id);
        qm_create(self, nix_hash, commit_hash)?;
        let disk_ref = qm_importdisk(self.vm_id, &qcow2_path, &self.storage_location)?;
        qm_set_disk(self.vm_id, &disk_ref, &self.disk_slot)?;
        qm_set_agent(self.vm_id)?;
        qm_resize(self.vm_id, &self.disk_slot, self.disk_gb)?;
        info!("VM {} provisioned successfully, starting", self.name);
        qm_start(self.vm_id)?;
        info!("VM {} started", self.name);

        Ok(())
    }
}

impl Materialise for ContainerConfig {
    fn nix_build_attr(&self) -> &str {
        "config.system.build.tarball"
    }
    fn provision(&self, artifact_path: &str, commit_hash: &str) -> Result<()> {
        let ostemplate = copy_to_template_storage(artifact_path)?;
        let nix_hash = nix_store_hash(artifact_path).ok_or_else(|| {
            AppError::CmdError(format!("could not extract nix hash from path {}", artifact_path))
        })?;
        info!("Provisioning container {} (id: {})", self.name, self.ct_id);
        pct_create(self, &ostemplate, nix_hash, commit_hash)?;
        pct_start(self.ct_id)?;
        info!("Container {} started", self.name);
        Ok(())
    }
}

use std::{env, path::PathBuf};

fn nyx_data_dir() -> PathBuf {
    env::var_os("ATROPOS_NYX_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".nyx")))
        .unwrap_or_else(|| env::temp_dir().join("atropos-libafl-nyx"))
}

pub fn project_dir() -> PathBuf {
    env::var_os("ATROPOS_PROJECT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

pub fn output_dir() -> PathBuf {
    env::var_os("ATROPOS_OUTPUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| wordpress_root().join("atropos-output"))
}

pub fn wordpress_root() -> PathBuf {
    env::var_os("ATROPOS_WORDPRESS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| project_dir().join("../wordpress"))
}

pub fn openapi_path() -> Option<PathBuf> {
    env::var_os("ATROPOS_OPENAPI")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn seed_dir() -> Option<PathBuf> {
    env::var_os("ATROPOS_SEED_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

pub fn nyx_share_dir() -> PathBuf {
    env::var_os("ATROPOS_NYX_SHARE")
        .map(PathBuf::from)
        .unwrap_or_else(|| nyx_data_dir().join("share"))
}

pub fn nyx_vm_dir() -> PathBuf {
    env::var_os("ATROPOS_NYX_VM_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| nyx_data_dir().join("vm"))
}

pub fn nyx_guest_artifact_dir() -> PathBuf {
    env::var_os("ATROPOS_NYX_GUEST_ARTIFACTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| nyx_data_dir().join("guest"))
}

pub fn nyx_php_cli() -> PathBuf {
    env::var_os("ATROPOS_NYX_PHP_PREFIX")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(".local/opt/atropos-libafl-nyx-php")
        })
        .join("bin/php")
}

pub fn phpcov_binary() -> PathBuf {
    env::var_os("ATROPOS_PHPCOV_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| nyx_guest_artifact_dir().join("php-code-coverage/vendor/bin/phpcov"))
}

pub fn nyx_vm_image() -> PathBuf {
    env::var_os("ATROPOS_NYX_VM_IMAGE")
        .map(PathBuf::from)
        .unwrap_or_else(|| nyx_vm_dir().join("atropos-nyx.qcow2"))
}

pub fn nyx_presnapshot() -> PathBuf {
    env::var_os("ATROPOS_NYX_PRESNAPSHOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| nyx_vm_dir().join("presnapshot"))
}

pub fn nyx_workdir_dir() -> PathBuf {
    env::var_os("ATROPOS_NYX_WORKDIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| nyx_data_dir().join("workdir"))
}

pub fn nyx_cpu_id() -> usize {
    env::var("ATROPOS_NYX_CPU")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
}

//! OS-level sandbox for the Bash tool (#470): macOS Seatbelt profiles.
//!
//! The SBPL files under `sandbox/` and the shape of the generated profile are
//! adapted from OpenAI Codex (`codex-rs/sandboxing/src/seatbelt.rs` and its
//! `seatbelt_*.sbpl` policies), Copyright OpenAI, licensed under the Apache
//! License 2.0 (http://www.apache.org/licenses/LICENSE-2.0).
//!
//! A profile denies everything by default, allows reading the whole disk,
//! allows writes only beneath the writable roots (minus the protected `.git`
//! and `.claurst` entries), and allows network access only when asked to.
//! Codex's fourth policy, `seatbelt_read_only_platform_defaults.sbpl`, is not
//! vendored: it only matters when reads are restricted, and here they never are.
//! The root paths are passed to `sandbox-exec` as `-D` parameters, never
//! spliced into the policy text.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

const SEATBELT_BASE_POLICY: &str = include_str!("sandbox/seatbelt_base_policy.sbpl");
const SEATBELT_NETWORK_POLICY: &str = include_str!("sandbox/seatbelt_network_policy.sbpl");
const SEATBELT_PREFERENCES_POLICY: &str = include_str!("sandbox/seatbelt_preferences_policy.sbpl");
/// System libcurl needs this service for TLS (codex `seatbelt.rs`).
const SEATBELT_TLS_TRUST_POLICY: &str =
    "(allow mach-lookup (global-name \"com.apple.TrustEvaluationAgent\"))\n";

/// Entries kept read-only directly beneath every writable root: the
/// repository metadata and claurst's own project settings.
const PROTECTED_NAMES: [&str; 2] = [".git", ".claurst"];

/// A generated Seatbelt profile plus the `-D` parameters it refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeatbeltProfile {
    policy: String,
    params: Vec<(String, PathBuf)>,
}

impl SeatbeltProfile {
    /// Build the profile for `writable_roots` (empty: read-only). Writes to
    /// any path in `protected` (and anything beneath it) stay denied inside
    /// every root that contains it. Paths must already be canonical: Seatbelt
    /// matches the resolved path, so `/var/...` never matches `/private/var/...`.
    pub fn new(writable_roots: &[PathBuf], protected: &[PathBuf], network: bool) -> Self {
        let mut params = Vec::new();
        let mut sections = vec![
            SEATBELT_BASE_POLICY.to_string(),
            "; allow read-only file operations\n(allow file-read*)".to_string(),
        ];

        if !writable_roots.is_empty() {
            let mut allows = Vec::with_capacity(writable_roots.len());
            let mut anchor_denies = Vec::with_capacity(writable_roots.len());
            for (index, root) in writable_roots.iter().enumerate() {
                let key = format!("WRITABLE_ROOT_{index}");
                params.push((key.clone(), root.clone()));
                // A root must not be replaced (renamed away and recreated)
                // between commands, or the next profile would grant a new tree.
                anchor_denies.push(format!(
                    "(deny file-write-unlink (require-all (literal (param \"{key}\")) (vnode-type DIRECTORY)))"
                ));
                let mut parts = vec![format!("(subpath (param \"{key}\"))")];
                let excluded = protected
                    .iter()
                    .filter(|path| path.starts_with(root) && path.as_path() != root.as_path());
                for (excluded_index, path) in excluded.enumerate() {
                    let excluded_key = format!("{key}_EXCLUDED_{excluded_index}");
                    // `literal` too: `subpath` alone would still allow creating
                    // the protected entry itself (`mkdir .git`).
                    parts.push(format!("(require-not (literal (param \"{excluded_key}\")))"));
                    parts.push(format!("(require-not (subpath (param \"{excluded_key}\")))"));
                    params.push((excluded_key, path.clone()));
                }
                allows.push(if parts.len() == 1 {
                    parts.remove(0)
                } else {
                    format!("(require-all {} )", parts.join(" "))
                });
            }
            sections.push(format!("(allow file-write*\n{}\n)", allows.join(" ")));
            sections.extend(anchor_denies);
        }

        if network {
            sections.push(format!(
                "(allow network-outbound)\n(allow network-inbound)\n{SEATBELT_NETWORK_POLICY}{SEATBELT_TLS_TRUST_POLICY}"
            ));
        }
        sections.push(SEATBELT_PREFERENCES_POLICY.to_string());
        sections.push("(deny mach-lookup (xpc-service-name-prefix \"\"))".to_string());
        // These fcntls mutate files through read-only descriptors, bypassing
        // file-write*. F_MAKECOMPRESSED = 80, F_TRANSFEREXTENTS = 110.
        sections.push("(deny system-fcntl (fcntl-command 80 110))".to_string());

        Self {
            policy: sections.join("\n"),
            params,
        }
    }

    /// The SBPL policy text passed with `-p`.
    pub fn policy(&self) -> &str {
        &self.policy
    }

    /// `sandbox-exec` arguments up to and including the `--` separator; the
    /// command to run follows them.
    pub fn args(&self) -> Vec<OsString> {
        let mut args = Vec::with_capacity(self.params.len() + 3);
        args.push(OsString::from("-p"));
        args.push(OsString::from(&self.policy));
        for (key, path) in &self.params {
            let mut arg = OsString::from(format!("-D{key}="));
            arg.push(path);
            args.push(arg);
        }
        args.push(OsString::from("--"));
        args
    }
}

/// The protected entries for `roots`: `<root>/.git` and `<root>/.claurst`,
/// plus the resolved target when such an entry is a symlink (a `.git` file
/// or link pointing elsewhere must not become writable through the link).
pub fn protected_paths(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    for root in roots {
        for name in PROTECTED_NAMES {
            let path = root.join(name);
            if let Ok(resolved) = std::fs::canonicalize(&path) {
                if resolved != path && !paths.contains(&resolved) {
                    paths.push(resolved);
                }
            }
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    paths
}

/// Canonicalize `path` for a profile, keeping it as given when it does not
/// exist (a missing root still gets a correct, if unusable, rule).
pub fn canonical_root(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything after the base policy and the file-read section.
    fn dynamic_part(profile: &SeatbeltProfile) -> &str {
        let marker = "(allow file-read*)\n";
        let start = profile.policy().find(marker).unwrap() + marker.len();
        &profile.policy()[start..]
    }

    fn arg_strings(profile: &SeatbeltProfile) -> Vec<String> {
        profile
            .args()
            .into_iter()
            .map(|arg| arg.into_string().unwrap())
            .collect()
    }

    const TAIL: &str = "(deny mach-lookup (xpc-service-name-prefix \"\"))\n\
                        (deny system-fcntl (fcntl-command 80 110))";

    #[test]
    fn profile_starts_deny_default_and_reads_everything() {
        let profile = SeatbeltProfile::new(&[], &[], false);
        let policy = profile.policy();
        assert!(policy.contains("(version 1)"));
        assert!(policy.contains("(deny default)"));
        assert!(policy.contains("Apache License 2.0"));
        assert!(policy.contains("(allow file-read*)"));
        assert!(policy.ends_with(TAIL));
    }

    #[test]
    fn snapshot_read_only_without_network() {
        let profile = SeatbeltProfile::new(&[], &[], false);
        let expected = format!("{SEATBELT_PREFERENCES_POLICY}\n{TAIL}");
        assert_eq!(dynamic_part(&profile), expected);
        assert!(!dynamic_part(&profile).contains("file-write*"));
        assert!(!dynamic_part(&profile).contains("network"));
        assert_eq!(arg_strings(&profile)[2..], ["--"]);
    }

    #[test]
    fn snapshot_workspace_write_with_protected_entries() {
        let roots = vec![PathBuf::from("/work/repo"), PathBuf::from("/private/tmp")];
        let protected = protected_paths(&roots);
        assert_eq!(
            protected,
            [
                "/work/repo/.git",
                "/work/repo/.claurst",
                "/private/tmp/.git",
                "/private/tmp/.claurst",
            ]
            .map(PathBuf::from)
        );
        let profile = SeatbeltProfile::new(&roots, &protected, false);
        let expected = format!(
            "(allow file-write*\n\
             (require-all (subpath (param \"WRITABLE_ROOT_0\")) \
             (require-not (literal (param \"WRITABLE_ROOT_0_EXCLUDED_0\"))) \
             (require-not (subpath (param \"WRITABLE_ROOT_0_EXCLUDED_0\"))) \
             (require-not (literal (param \"WRITABLE_ROOT_0_EXCLUDED_1\"))) \
             (require-not (subpath (param \"WRITABLE_ROOT_0_EXCLUDED_1\"))) ) \
             (require-all (subpath (param \"WRITABLE_ROOT_1\")) \
             (require-not (literal (param \"WRITABLE_ROOT_1_EXCLUDED_0\"))) \
             (require-not (subpath (param \"WRITABLE_ROOT_1_EXCLUDED_0\"))) \
             (require-not (literal (param \"WRITABLE_ROOT_1_EXCLUDED_1\"))) \
             (require-not (subpath (param \"WRITABLE_ROOT_1_EXCLUDED_1\"))) )\n\
             )\n\
             (deny file-write-unlink (require-all (literal (param \"WRITABLE_ROOT_0\")) (vnode-type DIRECTORY)))\n\
             (deny file-write-unlink (require-all (literal (param \"WRITABLE_ROOT_1\")) (vnode-type DIRECTORY)))\n\
             {SEATBELT_PREFERENCES_POLICY}\n{TAIL}"
        );
        assert_eq!(dynamic_part(&profile), expected);
        let args = arg_strings(&profile);
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], profile.policy());
        assert_eq!(
            args[2..],
            [
                "-DWRITABLE_ROOT_0=/work/repo",
                "-DWRITABLE_ROOT_0_EXCLUDED_0=/work/repo/.git",
                "-DWRITABLE_ROOT_0_EXCLUDED_1=/work/repo/.claurst",
                "-DWRITABLE_ROOT_1=/private/tmp",
                "-DWRITABLE_ROOT_1_EXCLUDED_0=/private/tmp/.git",
                "-DWRITABLE_ROOT_1_EXCLUDED_1=/private/tmp/.claurst",
                "--",
            ]
        );
    }

    /// A workspace inside another writable root (a repo under TMPDIR): the
    /// outer root's rule must also exclude the inner root's `.git`, or the
    /// union of the two rules would leave it writable.
    #[test]
    fn snapshot_nested_root_inherits_inner_protected_entries() {
        let roots = vec![PathBuf::from("/tmp/T/repo"), PathBuf::from("/tmp/T")];
        let protected = vec![PathBuf::from("/tmp/T/repo/.git")];
        let profile = SeatbeltProfile::new(&roots, &protected, false);
        assert!(dynamic_part(&profile).starts_with(
            "(allow file-write*\n\
             (require-all (subpath (param \"WRITABLE_ROOT_0\")) \
             (require-not (literal (param \"WRITABLE_ROOT_0_EXCLUDED_0\"))) \
             (require-not (subpath (param \"WRITABLE_ROOT_0_EXCLUDED_0\"))) ) \
             (require-all (subpath (param \"WRITABLE_ROOT_1\")) \
             (require-not (literal (param \"WRITABLE_ROOT_1_EXCLUDED_0\"))) \
             (require-not (subpath (param \"WRITABLE_ROOT_1_EXCLUDED_0\"))) )\n)"
        ));
        assert_eq!(
            arg_strings(&profile)[2..],
            [
                "-DWRITABLE_ROOT_0=/tmp/T/repo",
                "-DWRITABLE_ROOT_0_EXCLUDED_0=/tmp/T/repo/.git",
                "-DWRITABLE_ROOT_1=/tmp/T",
                "-DWRITABLE_ROOT_1_EXCLUDED_0=/tmp/T/repo/.git",
                "--",
            ]
        );
    }

    #[test]
    fn snapshot_root_without_protected_entries_is_a_plain_subpath() {
        let profile = SeatbeltProfile::new(&[PathBuf::from("/work")], &[], false);
        assert!(dynamic_part(&profile).starts_with(
            "(allow file-write*\n(subpath (param \"WRITABLE_ROOT_0\"))\n)\n\
             (deny file-write-unlink (require-all (literal (param \"WRITABLE_ROOT_0\")) (vnode-type DIRECTORY)))\n"
        ));
    }

    #[test]
    fn snapshot_network_section_only_when_enabled() {
        let roots = [PathBuf::from("/work")];
        let off = SeatbeltProfile::new(&roots, &[], false);
        let on = SeatbeltProfile::new(&roots, &[], true);
        assert!(!dynamic_part(&off).contains("network"));
        let network = format!(
            "(allow network-outbound)\n(allow network-inbound)\n{SEATBELT_NETWORK_POLICY}{SEATBELT_TLS_TRUST_POLICY}\n{SEATBELT_PREFERENCES_POLICY}\n{TAIL}"
        );
        assert!(dynamic_part(&on).ends_with(&network));
        assert_eq!(off.args()[2..], on.args()[2..]);
    }

    #[cfg(unix)]
    #[test]
    fn protected_paths_add_the_target_of_a_symlinked_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = canonical_root(dir.path());
        let elsewhere = root.join("gitdir");
        std::fs::create_dir(&elsewhere).unwrap();
        std::os::unix::fs::symlink(&elsewhere, root.join(".git")).unwrap();
        let protected = protected_paths(std::slice::from_ref(&root));
        assert_eq!(
            protected,
            [elsewhere, root.join(".git"), root.join(".claurst")]
        );
    }
}

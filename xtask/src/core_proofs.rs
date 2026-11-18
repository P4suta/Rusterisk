use crate::{process::ProcessError, reference::Step};
use serde::Deserialize;
use std::time::Duration;
mod capture;
mod runner;
pub use capture::CapturedCoreProofs;
#[cfg(all(test, unix))]
pub(crate) use runner::artifact_fixture;
pub use runner::{execute, plan, CoreProofOutcome, Execution};
#[cfg(unix)]
pub(crate) use runner::{read_retained, verify_retained, CommandMode};

pub const HARNESSES: [&str; 18] = [
    "proofs::terminal_release_is_absorbing",
    "proofs::connection_claims_are_exclusive",
    "proofs::stasis_tracks_the_current_app",
    "proofs::bridge_membership_is_exclusive",
    "proofs::cancellation_releases_every_media_slot",
    "proofs::media_tokens_do_not_cross_generations",
    "proofs::transfer_completion_is_absorbing",
    "proofs::transfer_observations_stay_in_their_generation",
    "proofs::transfer_detachment_preserves_live_calls",
    "proofs::attended_handoff_preserves_calls_and_bridge_ownership",
    "proofs::attended_progress_stays_in_the_current_generation",
    "proofs::attended_completion_is_absorbing",
    "proofs::owned_pair_departures_stay_with_the_current_bridge_and_generations",
    "proofs::owned_pair_release_is_absorbing",
    "proofs::settings_references_preserve_their_generation_until_release",
    "proofs::settings_candidates_cannot_cross_domains_or_replace_later_candidates",
    "proofs::settings_foreign_references_leave_both_domains_unchanged",
    "proofs::settings_publication_preserves_saturated_references",
];

#[derive(Deserialize)]
struct Manifest {
    lib: Library,
    #[serde(default)]
    dependencies: std::collections::BTreeMap<String, serde::de::IgnoredAny>,
}

#[derive(Deserialize)]
struct Library {
    name: String,
    path: String,
}

pub fn validate_manifest(source: &str) -> Result<(), ProofError> {
    let manifest: Manifest = toml::from_str(source).map_err(|_| ProofError::Plan)?;
    if manifest.lib.name != "rusterisk_core"
        || manifest.lib.path != "../src/lib.rs"
        || !manifest.dependencies.is_empty()
    {
        return Err(ProofError::Plan);
    }
    Ok(())
}

#[derive(Debug)]
pub enum ProofError {
    Plan,
    Results,
    Harnesses,
    Syntax(syn::Error),
    Io(std::io::Error),
    Json(serde_json::Error),
    Process(ProcessError),
    Cleanup {
        operation: String,
        cleanup: ProcessError,
    },
    SourceChanged,
    FollowUp {
        operation: String,
        context: &'static str,
        follow_up: String,
    },
}

impl std::fmt::Display for ProofError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Plan => formatter.write_str("invalid bounded core proof plan"),
            Self::Results => formatter.write_str(
                "core proofs require complete Kani 0.68 and CBMC 6.11 results with reachable contracts",
            ),
            Self::Harnesses => formatter.write_str("core harnesses require the declared scope, explicit bounds, assertions, and cover goals"),
            Self::Syntax(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
            Self::Json(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Cleanup { operation, cleanup } => {
                write!(formatter, "{operation}; proof cleanup failed: {cleanup}")
            }
            Self::SourceChanged => formatter.write_str("captured core proof inputs changed"),
            Self::FollowUp { operation, context, follow_up } => write!(formatter,"{operation}; {context}: {follow_up}"),
        }
    }
}
impl std::error::Error for ProofError {}
impl From<std::io::Error> for ProofError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<ProcessError> for ProofError {
    fn from(error: ProcessError) -> Self {
        Self::Process(error)
    }
}

pub struct CoreProofPlan {
    name: String,
    source: String,
    results: String,
    image: crate::reference::ImageIdentity,
}

impl CoreProofPlan {
    pub fn image(&self) -> &str {
        self.image.as_str()
    }

    pub fn new(
        identity: &str,
        source: &str,
        results: &str,
        image: &str,
    ) -> Result<Self, ProofError> {
        let image = crate::reference::ImageIdentity::parse(image).ok_or(ProofError::Plan)?;
        if identity.is_empty()
            || identity.len() > 64
            || identity.starts_with('-')
            || !identity
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            || [source, results].iter().any(|path| {
                path.is_empty() || path.len() > 4096 || path.contains([',', '\r', '\n', '\0'])
            })
        {
            return Err(ProofError::Plan);
        }
        Ok(Self {
            name: format!("rusterisk-core-proofs-{identity}"),
            source: source.into(),
            results: results.into(),
            image,
        })
    }

    pub fn run(&self) -> Step {
        let mut arguments: Vec<String> = [
            "run",
            "--rm",
            "--pull=never",
            "--name",
            &self.name,
            "--cpus=2",
            "--memory=2g",
            "--memory-swap=2g",
            "--pids-limit=256",
            "--network=none",
            "--read-only",
            "--cap-drop=ALL",
            "--security-opt=no-new-privileges",
            "--ulimit",
            "fsize=134217728:134217728",
            "--tmpfs",
            "/tmp:mode=1777,size=768m",
            "--mount",
            &format!("type=bind,source={},target=/project,readonly", self.source),
            "--mount",
            &format!("type=bind,source={},target=/results", self.results),
            "--workdir=/tmp",
            "--env=CARGO_HOME=/tmp/cargo",
            "--env=CARGO_BUILD_JOBS=1",
            "--env=CARGO_NET_OFFLINE=true",
            "--entrypoint=/usr/bin/timeout",
            self.image.as_str(),
            "--signal=TERM",
            "--kill-after=10s",
            "880s",
            "cargo",
            "kani",
            "--manifest-path",
            "/project/proofs/Cargo.toml",
            "--lib",
            "--target-dir=/tmp/target",
            "-Z",
            "unstable-options",
            "--jobs=1",
            "--harness-timeout=120s",
            "--output-format=terse",
            "--export-json=/results/kani.json",
            "--exact",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect();
        for name in HARNESSES {
            arguments.push("--harness".into());
            arguments.push(name.into());
        }
        Step {
            arguments,
            budget: Duration::from_secs(900),
        }
    }

    pub fn cleanup(&self) -> Step {
        Step {
            arguments: ["rm", "--force", &self.name]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            budget: Duration::from_secs(30),
        }
    }
}

#[derive(Deserialize)]
struct Export {
    metadata: Metadata,
    tools: Tools,
    verification_results: Verification,
}
#[derive(Deserialize)]
struct Tools {
    kani: String,
    cbmc: String,
}
#[derive(Deserialize)]
struct Metadata {
    version: String,
    kani_version: String,
}
#[derive(Deserialize)]
struct Verification {
    summary: Summary,
    results: Vec<HarnessResult>,
}
#[derive(Deserialize)]
struct Summary {
    total_harnesses: usize,
    executed: usize,
    successful: usize,
    failed: usize,
    status: String,
}
#[derive(Deserialize)]
struct HarnessResult {
    harness_id: String,
    status: HarnessStatus,
    checks: Vec<Check>,
}
#[derive(Deserialize, PartialEq, Eq)]
enum HarnessStatus {
    Success,
    Failure,
}
#[derive(Deserialize)]
struct Check {
    status: CheckStatus,
    category: String,
}
#[derive(Deserialize, PartialEq, Eq)]
enum CheckStatus {
    Success,
    Failure,
    Unreachable,
    Undetermined,
    Unknown,
    Error,
    Satisfied,
    Unsatisfiable,
    Covered,
    Uncovered,
}

pub fn validate_results(bytes: &[u8]) -> Result<(), ProofError> {
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(ProofError::Results);
    }
    let export: Export = serde_json::from_slice(bytes).map_err(ProofError::Json)?;
    let verification = export.verification_results;
    let summary = verification.summary;
    if export.metadata.version != "1.0"
        || export.metadata.kani_version != "0.68.0"
        || export.tools.kani != "0.68.0"
        || export.tools.cbmc.split_whitespace().next() != Some("6.11.0")
        || summary.status != "completed"
        || summary.total_harnesses != HARNESSES.len()
        || summary.executed != HARNESSES.len()
        || summary.successful != HARNESSES.len()
        || summary.failed != 0
        || verification.results.len() != HARNESSES.len()
    {
        return Err(ProofError::Results);
    }
    let mut seen = std::collections::BTreeSet::new();
    for result in verification.results {
        if result.status != HarnessStatus::Success
            || !HARNESSES.contains(&result.harness_id.as_str())
            || !seen.insert(result.harness_id)
            || result.checks.is_empty()
            || result.checks.len() > 65536
        {
            return Err(ProofError::Results);
        }
        let mut assertion = false;
        let mut covered = false;
        for check in result.checks {
            if check.category == "cover"
                && !matches!(check.status, CheckStatus::Satisfied | CheckStatus::Covered)
            {
                return Err(ProofError::Results);
            }
            match check.status {
                CheckStatus::Success
                | CheckStatus::Unreachable
                | CheckStatus::Satisfied
                | CheckStatus::Covered => {}
                CheckStatus::Failure
                | CheckStatus::Undetermined
                | CheckStatus::Unknown
                | CheckStatus::Error
                | CheckStatus::Unsatisfiable
                | CheckStatus::Uncovered => return Err(ProofError::Results),
            }
            assertion |= check.category == "assertion" && check.status == CheckStatus::Success;
            covered |= check.category == "cover"
                && matches!(check.status, CheckStatus::Satisfied | CheckStatus::Covered);
        }
        if !assertion || !covered {
            return Err(ProofError::Results);
        }
    }
    Ok(())
}

fn kani_attribute(attribute: &syn::Attribute, name: &str) -> bool {
    attribute.path().segments.len() == 2
        && attribute.path().segments[0].ident == "kani"
        && attribute.path().segments[1].ident == name
}

#[derive(Default)]
struct HarnessBody {
    assertions: usize,
    covers: usize,
    forbidden: bool,
}

impl<'ast> syn::visit::Visit<'ast> for HarnessBody {
    fn visit_ident(&mut self, ident: &'ast syn::Ident) {
        if matches!(
            ident.to_string().as_str(),
            "assume" | "stub" | "stub_verified" | "should_panic"
        ) {
            self.forbidden = true;
        }
    }

    fn visit_macro(&mut self, value: &'ast syn::Macro) {
        if value.path.is_ident("assert")
            || value.path.is_ident("assert_eq")
            || value.path.is_ident("assert_ne")
        {
            self.assertions += 1;
        }
        if value.path.segments.len() == 2
            && value.path.segments[0].ident == "kani"
            && value.path.segments[1].ident == "cover"
        {
            self.covers += 1;
        }
        syn::visit::visit_macro(self, value);
    }
}

pub fn validate_harnesses(source: &str) -> Result<(), ProofError> {
    use syn::visit::Visit;
    let parsed = syn::parse_file(source).map_err(ProofError::Syntax)?;
    let mut all = HarnessBody::default();
    all.visit_file(&parsed);
    if all.forbidden {
        return Err(ProofError::Harnesses);
    }
    let mut names = std::collections::BTreeSet::new();
    for item in parsed.items {
        if let syn::Item::Fn(function) = item {
            if !function
                .attrs
                .iter()
                .any(|attribute| kani_attribute(attribute, "proof"))
            {
                continue;
            }
            let name = format!("proofs::{}", function.sig.ident);
            let settings = matches!(
                name.as_str(),
                "proofs::settings_references_preserve_their_generation_until_release"
                    | "proofs::settings_candidates_cannot_cross_domains_or_replace_later_candidates"
                    | "proofs::settings_foreign_references_leave_both_domains_unchanged"
                    | "proofs::settings_publication_preserves_saturated_references"
            );
            let bounded = function.attrs.iter().any(|attribute| {
                kani_attribute(attribute, "unwind")
                    && attribute
                        .parse_args::<syn::LitInt>()
                        .ok()
                        .is_some_and(|value| {
                            value.base10_parse::<u16>().is_ok_and(|bound| {
                                if settings {
                                    bound == 66
                                } else {
                                    (1..=32).contains(&bound)
                                }
                            })
                        })
            });
            let mut body = HarnessBody::default();
            body.visit_block(&function.block);
            if !bounded
                || body.assertions == 0
                || body.covers == 0
                || !HARNESSES.contains(&name.as_str())
                || !names.insert(name)
            {
                return Err(ProofError::Harnesses);
            }
        }
    }
    if names.len() != HARNESSES.len() {
        return Err(ProofError::Harnesses);
    }
    Ok(())
}

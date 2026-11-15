use super::AssuranceError;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Invariant {
    Terminal,
    Connection,
    Stasis,
    Bridge,
    MediaTasks,
    Subscriptions,
}

impl Invariant {
    const ALL: [Self; 6] = [
        Self::Terminal,
        Self::Connection,
        Self::Stasis,
        Self::Bridge,
        Self::MediaTasks,
        Self::Subscriptions,
    ];

    fn of(file: &str, function: &str) -> Option<Self> {
        match (file, function) {
            ("src/lib.rs", "Call::apply") => Some(Self::Terminal),
            ("src/lib.rs", "Call::claim_connection") => Some(Self::Connection),
            (
                "src/stasis.rs",
                "Call::reserve_stasis"
                | "Call::enter_stasis"
                | "Call::move_stasis"
                | "Call::leave_stasis",
            ) => Some(Self::Stasis),
            ("src/lib.rs", "Call::join" | "Call::leave") => Some(Self::Bridge),
            (
                "src/media_tasks.rs",
                "MediaTasks::reserve"
                | "MediaTasks::index"
                | "MediaTasks::cancel"
                | "MediaTasks::finish",
            ) => Some(Self::MediaTasks),
            ("src/resource_subscriptions.rs", function)
                if function.starts_with("ResourceSubscriptions")
                    && ["::subscribe", "::unsubscribe", "::expire"]
                        .iter()
                        .any(|suffix| function.ends_with(suffix)) =>
            {
                Some(Self::Subscriptions)
            }
            _ => None,
        }
    }

    const fn label(self) -> &'static str {
        match self {
            Self::Terminal => "terminal cleanup",
            Self::Connection => "connection claim",
            Self::Stasis => "Stasis application",
            Self::Bridge => "bridge exclusivity",
            Self::MediaTasks => "media ownership",
            Self::Subscriptions => "subscription bounds",
        }
    }
}

#[derive(Clone, Deserialize, PartialEq, Eq)]
struct Function {
    function_name: String,
}
#[derive(Clone, Deserialize, PartialEq, Eq)]
struct Mutant {
    name: String,
    file: String,
    function: Option<Function>,
    replacement: String,
}

impl Mutant {
    fn invariant(&self) -> Option<Invariant> {
        Invariant::of(&self.file, &self.function.as_ref()?.function_name)
    }
}

pub struct MutationInventory {
    mutants: BTreeMap<String, Mutant>,
}

fn json<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, AssuranceError> {
    if bytes.is_empty() || bytes.len() > 4 * 1024 * 1024 {
        return Err(AssuranceError::Results);
    }
    serde_json::from_slice(bytes).map_err(AssuranceError::Json)
}

pub fn validate_mutation_inventory(bytes: &[u8]) -> Result<MutationInventory, AssuranceError> {
    let values: Vec<Mutant> = json(bytes)?;
    if values.is_empty() {
        return Err(AssuranceError::EmptyMutationInventory);
    }
    if values.len() > 512 {
        return Err(AssuranceError::Inventory);
    }
    let mut mutants = BTreeMap::new();
    let mut covered = BTreeSet::new();
    for mutant in values {
        let invariant = mutant
            .invariant()
            .ok_or_else(|| AssuranceError::UnknownMutationScope {
                file: mutant.file.clone(),
                function: mutant
                    .function
                    .as_ref()
                    .map(|function| function.function_name.clone()),
            })?;
        if mutant.name.is_empty()
            || mutant.name.len() > 2048
            || mutant.replacement.len() > 16384
            || mutants.insert(mutant.name.clone(), mutant).is_some()
        {
            return Err(AssuranceError::Inventory);
        }
        covered.insert(invariant);
    }
    let missing: Vec<&'static str> = Invariant::ALL
        .iter()
        .filter(|invariant| !covered.contains(invariant))
        .map(|invariant| invariant.label())
        .collect();
    if !missing.is_empty() {
        return Err(AssuranceError::MissingMutationScopes(missing));
    }
    for invariant in Invariant::ALL {
        let count = mutants
            .values()
            .filter(|mutant| mutant.invariant() == Some(invariant))
            .count();
        println!("mutation inventory: {}: {count}", invariant.label());
    }
    Ok(MutationInventory { mutants })
}

#[derive(Deserialize)]
struct Lab {
    cargo_mutants_version: String,
    end_time: Option<String>,
    total_mutants: usize,
    missed: usize,
    caught: usize,
    timeout: usize,
    unviable: usize,
    success: usize,
    outcomes: Vec<Outcome>,
}
#[derive(Deserialize)]
struct Outcome {
    scenario: Scenario,
    summary: Summary,
    phase_results: Vec<PhaseResult>,
}
#[derive(Deserialize)]
enum Scenario {
    Baseline,
    Mutant(Mutant),
}
#[derive(Deserialize, PartialEq, Eq)]
enum Summary {
    Success,
    CaughtMutant,
    MissedMutant,
    Unviable,
    Failure,
    Timeout,
}
#[derive(Deserialize)]
struct PhaseResult {
    phase: Phase,
    process_status: Exit,
}
#[derive(Deserialize, PartialEq, Eq)]
enum Phase {
    Check,
    Build,
    Test,
}
#[derive(Deserialize, PartialEq, Eq)]
enum Exit {
    Success,
    Failure(i32),
    Timeout,
    Signalled(i32),
    Other,
}

fn phases(outcome: &Outcome) -> Result<(&Exit, Option<&Exit>), AssuranceError> {
    let mut build = None;
    let mut test = None;
    for phase in &outcome.phase_results {
        if matches!(
            phase.process_status,
            Exit::Timeout | Exit::Other | Exit::Failure(0)
        ) || matches!(phase.process_status, Exit::Signalled(signal) if signal <= 0)
        {
            return Err(AssuranceError::Results);
        }
        match phase.phase {
            Phase::Build if build.is_none() && test.is_none() => {
                build = Some(&phase.process_status)
            }
            Phase::Test if build == Some(&Exit::Success) && test.is_none() => {
                test = Some(&phase.process_status)
            }
            Phase::Check | Phase::Build | Phase::Test => return Err(AssuranceError::Results),
        }
    }
    Ok((build.ok_or(AssuranceError::Results)?, test))
}

pub fn validate_mutation_outcomes(
    bytes: &[u8],
    inventory: &MutationInventory,
) -> Result<(), AssuranceError> {
    let lab: Lab = json(bytes)?;
    if lab.cargo_mutants_version != "27.1.0"
        || !lab.end_time.is_some_and(|time| !time.is_empty())
        || lab.total_mutants != inventory.mutants.len()
        || lab.outcomes.len() != inventory.mutants.len() + 1
        || lab.missed != 0
        || lab.timeout != 0
        || lab.success != 0
        || lab.caught == 0
        || lab.caught + lab.unviable != lab.total_mutants
    {
        return Err(AssuranceError::Results);
    }
    let mut baseline = false;
    let mut seen = BTreeSet::new();
    let mut caught = 0;
    let mut unviable = 0;
    let mut covered = BTreeSet::new();
    for outcome in lab.outcomes {
        let (build, test) = phases(&outcome)?;
        match &outcome.scenario {
            Scenario::Baseline => {
                if baseline
                    || outcome.summary != Summary::Success
                    || build != &Exit::Success
                    || test != Some(&Exit::Success)
                {
                    return Err(AssuranceError::Results);
                }
                baseline = true;
            }
            Scenario::Mutant(mutant) => {
                if !baseline
                    || inventory.mutants.get(&mutant.name) != Some(mutant)
                    || !seen.insert(mutant.name.clone())
                {
                    return Err(AssuranceError::Results);
                }
                match outcome.summary {
                    Summary::CaughtMutant
                        if build == &Exit::Success
                            && matches!(test, Some(Exit::Failure(code)) if *code != 0) =>
                    {
                        caught += 1;
                        covered.insert(mutant.invariant().ok_or(AssuranceError::Results)?);
                    }
                    Summary::Unviable
                        if matches!(build, Exit::Failure(code) if *code != 0) && test.is_none() =>
                    {
                        unviable += 1
                    }
                    Summary::Success
                    | Summary::CaughtMutant
                    | Summary::MissedMutant
                    | Summary::Unviable
                    | Summary::Failure
                    | Summary::Timeout => {
                        eprintln!("incomplete mutation: {}", mutant.name);
                        return Err(AssuranceError::Results);
                    }
                }
            }
        }
    }
    if !baseline
        || seen.len() != inventory.mutants.len()
        || caught != lab.caught
        || unviable != lab.unviable
        || Invariant::ALL
            .iter()
            .any(|invariant| !covered.contains(invariant))
    {
        return Err(AssuranceError::Results);
    }
    println!("core mutation: {caught} caught; {unviable} unviable; every declared invariant detected a viable mutation");
    Ok(())
}

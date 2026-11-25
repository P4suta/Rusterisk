use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::Path,
    time::Duration,
};

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "incomplete or inconsistent process measurement",
    )
}

fn read(path: &Path) -> io::Result<String> {
    let mut text = String::new();
    fs::File::open(path)?
        .take(65_537)
        .read_to_string(&mut text)?;
    if text.len() > 65_536 {
        return Err(invalid());
    }
    Ok(text)
}

fn field<'a>(text: &'a str, name: &str) -> io::Result<&'a str> {
    let mut values = text.lines().filter_map(|line| {
        let (label, value) = line.split_once(':')?;
        (label == name).then_some(value.trim())
    });
    let value = values.next().ok_or_else(invalid)?;
    if values.next().is_some() {
        return Err(invalid());
    }
    Ok(value)
}

fn counter(text: &str, name: &str) -> io::Result<u64> {
    field(text, name)?.parse().map_err(|_| invalid())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Switches {
    pub voluntary: u64,
    pub involuntary: u64,
}

struct Thread {
    started: u64,
    switches: Switches,
}

pub struct ProcessFootprint {
    pid: u32,
    threads: BTreeMap<u32, Thread>,
    pub anonymous_kib: u64,
    pub executable_bytes: u64,
}

pub struct Interval {
    pub threads: BTreeMap<u32, Switches>,
    total: u64,
    elapsed: Duration,
}

impl Interval {
    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn within_switch_budget(&self, per_minute: u64) -> bool {
        u128::from(self.total) * Duration::from_secs(60).as_nanos()
            <= u128::from(per_minute).saturating_mul(self.elapsed.as_nanos())
    }
}

fn task(root: &Path, tid: u32, pid: u32) -> io::Result<Thread> {
    let status = read(&root.join("status"))?;
    if counter(&status, "Pid")? != u64::from(tid) || counter(&status, "Tgid")? != u64::from(pid) {
        return Err(invalid());
    }
    let stat = read(&root.join("stat"))?;
    let (identity, fields) = stat.rsplit_once(") ").ok_or_else(invalid)?;
    if !identity.starts_with(&format!("{tid} (")) {
        return Err(invalid());
    }
    let started = fields
        .split_ascii_whitespace()
        .nth(19)
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    Ok(Thread {
        started,
        switches: Switches {
            voluntary: counter(&status, "voluntary_ctxt_switches")?,
            involuntary: counter(&status, "nonvoluntary_ctxt_switches")?,
        },
    })
}

impl ProcessFootprint {
    pub fn read(procfs: &Path, pid: u32) -> io::Result<Self> {
        let root = procfs.join(pid.to_string());
        let status = read(&root.join("status"))?;
        let count = counter(&status, "Threads")?;
        if counter(&status, "Pid")? != u64::from(pid) || !(1..=512).contains(&count) {
            return Err(invalid());
        }
        let mut threads = BTreeMap::new();
        for entry in fs::read_dir(root.join("task"))? {
            let entry = entry?;
            let tid = entry
                .file_name()
                .to_str()
                .and_then(|name| name.parse().ok())
                .ok_or_else(invalid)?;
            if threads.len() >= 512
                || threads
                    .insert(tid, task(&entry.path(), tid, pid)?)
                    .is_some()
            {
                return Err(invalid());
            }
        }
        if !threads.contains_key(&pid) || threads.len() as u64 != count {
            return Err(invalid());
        }
        let smaps = read(&root.join("smaps_rollup"))?;
        let anonymous_kib = field(&smaps, "Anonymous")?
            .strip_suffix(" kB")
            .ok_or_else(invalid)?
            .trim_end()
            .parse()
            .map_err(|_| invalid())?;
        Ok(Self {
            pid,
            threads,
            anonymous_kib,
            executable_bytes: fs::metadata(root.join("exe"))?.len(),
        })
    }

    pub fn since(&self, before: &Self, elapsed: Duration) -> io::Result<Interval> {
        if elapsed.is_zero()
            || before.pid != self.pid
            || before.executable_bytes != self.executable_bytes
            || self.threads.len() != before.threads.len()
        {
            return Err(invalid());
        }
        let mut threads = BTreeMap::new();
        let mut total = 0_u64;
        for (&tid, thread) in &self.threads {
            let first = before
                .threads
                .get(&tid)
                .filter(|first| first.started == thread.started)
                .ok_or_else(invalid)?;
            let switches = Switches {
                voluntary: thread
                    .switches
                    .voluntary
                    .checked_sub(first.switches.voluntary)
                    .ok_or_else(invalid)?,
                involuntary: thread
                    .switches
                    .involuntary
                    .checked_sub(first.switches.involuntary)
                    .ok_or_else(invalid)?,
            };
            total = total
                .checked_add(switches.voluntary)
                .and_then(|sum| sum.checked_add(switches.involuntary))
                .ok_or_else(invalid)?;
            threads.insert(tid, switches);
        }
        Ok(Interval {
            threads,
            total,
            elapsed,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Milestones {
    bob_originate_http_accepted: f64,
    both_browsers_answered: f64,
    #[serde(rename = "aliceObserved660HzTone")]
    alice: f64,
    #[serde(rename = "bobObserved440HzTone")]
    bob: f64,
}

#[derive(Deserialize)]
struct FirstCall {
    format: String,
    version: u8,
    clock: String,
    unit: String,
    origin: String,
    started: bool,
    milestones: Milestones,
}

pub fn first_bidirectional_audio_ms(record: &str) -> io::Result<f64> {
    if record.len() > 8192 {
        return Err(invalid());
    }
    let call: FirstCall = serde_json::from_str(record)?;
    let times = call.milestones;
    if call.format != "rusterisk-first-call-observations"
        || call.version != 1
        || call.clock != "node-performance-now"
        || call.unit != "milliseconds"
        || call.origin != "before-alice-call-click"
        || !call.started
        || !(0.0..=times.both_browsers_answered).contains(&times.bob_originate_http_accepted)
        || !(times.both_browsers_answered..=60_000.0).contains(&times.alice)
        || !(times.both_browsers_answered..=60_000.0).contains(&times.bob)
    {
        return Err(invalid());
    }
    Ok(times.alice.max(times.bob))
}

pub fn measure(pid: u32, seconds: u64, budget: Option<u64>) -> io::Result<()> {
    if !(1..=300).contains(&seconds) {
        return Err(io::Error::other(
            "sample duration must be between 1 and 300 seconds",
        ));
    }
    let procfs = Path::new("/proc");
    let before = ProcessFootprint::read(procfs, pid)?;
    let start = std::time::Instant::now();
    std::thread::sleep(Duration::from_secs(seconds));
    let after = ProcessFootprint::read(procfs, pid)?;
    let elapsed = start.elapsed();
    let interval = after.since(&before, elapsed)?;
    println!(
        "pid={pid} elapsed_ms={} anonymous_kib={} executable_bytes={} threads={} switches={}",
        elapsed.as_millis(),
        after.anonymous_kib,
        after.executable_bytes,
        interval.threads.len(),
        interval.total()
    );
    for (tid, switches) in &interval.threads {
        println!(
            "tid={tid} voluntary={} involuntary={}",
            switches.voluntary, switches.involuntary
        );
    }
    if budget.is_some_and(|limit| !interval.within_switch_budget(limit)) {
        return Err(io::Error::other(
            "idle context switches exceed the configured rate",
        ));
    }
    Ok(())
}

pub fn print_first_call(path: &Path) -> io::Result<()> {
    let record = if path == Path::new("-") {
        let mut record = String::new();
        io::stdin().lock().take(8193).read_to_string(&mut record)?;
        record
    } else {
        read(path)?
    };
    println!(
        "first_bidirectional_audio_ms={}",
        first_bidirectional_audio_ms(&record)?
    );
    Ok(())
}

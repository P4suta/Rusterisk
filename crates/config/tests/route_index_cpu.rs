#![cfg(target_os = "linux")]

use rusterisk_config::{Application, Dialplan, Step};
use std::{hint::black_box, io};

fn cpu_ticks(work: impl FnOnce()) -> io::Result<u64> {
    fn read() -> io::Result<u64> {
        let stat = std::fs::read_to_string("/proc/thread-self/stat")?;
        let (_, fields) = stat.rsplit_once(") ").ok_or(io::ErrorKind::InvalidData)?;
        let mut fields = fields.split_whitespace().skip(11);
        let mut ticks = || {
            fields
                .next()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or(io::ErrorKind::InvalidData)
        };
        Ok(ticks()? + ticks()?)
    }
    let start = read()?;
    work();
    read()?
        .checked_sub(start)
        .ok_or(io::ErrorKind::InvalidData.into())
}

#[test]
#[ignore = "Measures route lookup CPU time on an isolated Linux worker."]
fn the_index_reduces_lookup_cpu_for_4096_instructions() -> io::Result<()> {
    let routes = Dialplan {
        steps: (0..4096)
            .map(|n| Step {
                context: "internal".into(),
                extension: format!("{n:04}"),
                priority: 1,
                application: Application::Hangup,
            })
            .collect(),
    };
    let indexed = routes.clone().compile();
    let queries: Vec<_> = (0..4096)
        .map(|n| format!("{:04}", n * 253 % 4096))
        .collect();
    for query in &queries {
        assert_eq!(
            routes.position("internal", query, 1),
            indexed.position("internal", query, 1)
        );
    }
    let mut linear = [0; 5];
    let mut compiled = [0; 5];
    for round in 0..5 {
        linear[round] = cpu_ticks(|| {
            for query in &queries {
                black_box(routes.position("internal", black_box(query), 1));
            }
        })?;
        compiled[round] = cpu_ticks(|| {
            for query in &queries {
                black_box(indexed.position("internal", black_box(query), 1));
            }
        })?;
    }
    println!("4096 lookups: linear CPU ticks {linear:?}, indexed CPU ticks {compiled:?}");
    linear.sort_unstable();
    compiled.sort_unstable();
    assert!(compiled[2] < linear[2]);
    Ok(())
}

use std::net::{Ipv4Addr, SocketAddr};
use xtask::reference::{Interop, ReferenceError, SippAdapter};

fn argument<'a>(arguments: &'a [String], flag: &str) -> Option<&'a str> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
}

#[test]
fn both_adapters_use_the_same_finite_one_call_registration_scenario() -> Result<(), ReferenceError>
{
    let plan = Interop::new("fixture", "/workspace/reference")?;
    let local = Ipv4Addr::new(192, 168, 215, 2);
    for (adapter, target, file) in [
        (
            SippAdapter::Asterisk,
            SocketAddr::from((local, 5060)),
            "/tmp/rusterisk-sipp-asterisk.csv",
        ),
        (
            SippAdapter::Rusterisk,
            SocketAddr::from((Ipv4Addr::new(0, 250, 250, 254), 49152)),
            "/tmp/rusterisk-sipp-rusterisk.csv",
        ),
    ] {
        let step = plan.sipp_registration(adapter, target, local)?;
        let arguments = &step.arguments;
        assert_eq!(step.budget.as_secs(), 30);
        assert_eq!(
            &arguments[..4],
            ["exec", "--workdir", "/tmp", "rusterisk-interop-fixture"]
        );
        assert_eq!(
            &arguments[4..9],
            [
                "/usr/bin/timeout",
                "--kill-after=2s",
                "20s",
                "/usr/local/bin/sipp",
                &target.to_string()
            ]
        );
        for (flag, value) in [
            ("-sf", "/etc/asterisk/sipp-registration.xml"),
            ("-i", "192.168.215.2"),
            ("-p", "0"),
            ("-t", "u1"),
            ("-m", "1"),
            ("-l", "1"),
            ("-r", "1"),
            ("-timeout", "15s"),
            ("-recv_timeout", "2000"),
            ("-send_timeout", "2000"),
            ("-default_behaviors", "abortunexp"),
            ("-stat_delimiter", ";"),
            ("-stf", file),
            ("-fd", "1"),
            ("-au", "phone"),
            ("-ap", "test-password"),
        ] {
            assert_eq!(argument(arguments, flag), Some(value), "{flag}");
        }
        for flag in ["-bind_local", "-timeout_error", "-nostdin", "-trace_stat"] {
            assert_eq!(
                arguments
                    .iter()
                    .filter(|argument| *argument == flag)
                    .count(),
                1
            );
        }
        assert!(!arguments.iter().any(|argument| argument == "-bg"));
        assert!(!arguments.iter().any(|argument| argument == "-nd"));
        let statistics = plan.sipp_statistics(adapter);
        assert_eq!(statistics.budget.as_secs(), 10);
        assert_eq!(
            statistics.arguments,
            ["exec", "rusterisk-interop-fixture", "cat", "--", file]
        );
    }
    Ok(())
}

#[test]
fn sipp_commands_reject_unroutable_targets_and_nonprivate_sources() -> Result<(), ReferenceError> {
    let plan = Interop::new("fixture", "/workspace/reference")?;
    let local = Ipv4Addr::new(192, 168, 215, 2);
    for target in [
        SocketAddr::from((Ipv4Addr::UNSPECIFIED, 5060)),
        SocketAddr::from((Ipv4Addr::BROADCAST, 5060)),
        SocketAddr::from((Ipv4Addr::new(224, 0, 0, 1), 5060)),
        SocketAddr::from((local, 0)),
        SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 5060)),
    ] {
        assert!(plan
            .sipp_registration(SippAdapter::Asterisk, target, local)
            .is_err());
    }
    let target = SocketAddr::from((local, 5060));
    for local in [
        Ipv4Addr::UNSPECIFIED,
        Ipv4Addr::BROADCAST,
        Ipv4Addr::LOCALHOST,
        Ipv4Addr::new(224, 0, 0, 1),
        Ipv4Addr::new(203, 0, 113, 1),
    ] {
        assert!(plan
            .sipp_registration(SippAdapter::Rusterisk, target, local)
            .is_err());
    }
    Ok(())
}

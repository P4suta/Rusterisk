use xtask::reference::validate_sipp_statistics;

const HEADER: &str = "StartTime;LastResetTime;CurrentTime;ElapsedTime(P);ElapsedTime(C);TargetRate;CallRate(P);CallRate(C);IncomingCall(P);IncomingCall(C);OutgoingCall(P);OutgoingCall(C);TotalCallCreated;CurrentCall;SuccessfulCall(P);SuccessfulCall(C);FailedCall(P);FailedCall(C);FailedCannotSendMessage(P);FailedCannotSendMessage(C);FailedMaxUDPRetrans(P);FailedMaxUDPRetrans(C);FailedTcpConnect(P);FailedTcpConnect(C);FailedTcpClosed(P);FailedTcpClosed(C);FailedUnexpectedMessage(P);FailedUnexpectedMessage(C);FailedCallRejected(P);FailedCallRejected(C);FailedCmdNotSent(P);FailedCmdNotSent(C);FailedRegexpDoesntMatch(P);FailedRegexpDoesntMatch(C);FailedRegexpShouldntMatch(P);FailedRegexpShouldntMatch(C);FailedRegexpHdrNotFound(P);FailedRegexpHdrNotFound(C);FailedOutboundCongestion(P);FailedOutboundCongestion(C);FailedTimeoutOnRecv(P);FailedTimeoutOnRecv(C);FailedTimeoutOnSend(P);FailedTimeoutOnSend(C);FailedTestDoesntMatch(P);FailedTestDoesntMatch(C);FailedTestShouldntMatch(P);FailedTestShouldntMatch(C);FailedStrcmpDoesntMatch(P);FailedStrcmpDoesntMatch(C);FailedStrcmpShouldntMatch(P);FailedStrcmpShouldntMatch(C);OutOfCallMsgs(P);OutOfCallMsgs(C);DeadCallMsgs(P);DeadCallMsgs(C);Retransmissions(P);Retransmissions(C);AutoAnswered(P);AutoAnswered(C);Warnings(P);Warnings(C);FatalErrors(P);FatalErrors(C);WatchdogMajor(P);WatchdogMajor(C);WatchdogMinor(P);WatchdogMinor(C);CallLength(P);CallLength(C);CallLengthStDev(P);CallLengthStDev(C);";

fn row(changes: &[(&str, &str)]) -> String {
    let mut fields: Vec<&str> = HEADER.split_terminator(';').map(|_| "0").collect();
    for (name, value) in [
        ("StartTime", "2026-11-17T04:00:00.000000+09:00"),
        ("LastResetTime", "2026-11-17T04:00:00.000000+09:00"),
        ("CurrentTime", "2026-11-17T04:00:00.100000+09:00"),
        ("ElapsedTime(P)", "00:00:00:100000"),
        ("ElapsedTime(C)", "00:00:00:100000"),
        ("TargetRate", "1.000"),
        ("OutgoingCall(P)", "1"),
        ("OutgoingCall(C)", "1"),
        ("TotalCallCreated", "1"),
        ("SuccessfulCall(P)", "1"),
        ("SuccessfulCall(C)", "1"),
    ]
    .into_iter()
    .chain(changes.iter().copied())
    {
        if let Some(index) = HEADER.split_terminator(';').position(|field| field == name) {
            fields[index] = value;
        }
    }
    format!("{};\n", fields.join(";"))
}

fn statistics(rows: &[String]) -> Vec<u8> {
    format!("{HEADER}\n{}", rows.concat()).into_bytes()
}

#[test]
fn one_native_completed_call_is_accepted_after_all_live_calls_end() {
    assert!(matches!(
        validate_sipp_statistics(&statistics(&[row(&[])])),
        Ok(1)
    ));
    let progress = row(&[
        ("CurrentCall", "1"),
        ("SuccessfulCall(P)", "0"),
        ("SuccessfulCall(C)", "0"),
    ]);
    let initial = row(&[
        ("OutgoingCall(P)", "0"),
        ("OutgoingCall(C)", "0"),
        ("TotalCallCreated", "0"),
        ("SuccessfulCall(P)", "0"),
        ("SuccessfulCall(C)", "0"),
    ]);
    let complete = row(&[("OutgoingCall(P)", "0")]);
    assert!(matches!(
        validate_sipp_statistics(&statistics(&[initial, progress, complete])),
        Ok(1)
    ));
}

#[test]
fn skipped_incomplete_extra_and_failed_calls_are_rejected() {
    for changes in [
        &[("OutgoingCall(C)", "0"), ("TotalCallCreated", "0")][..],
        &[("SuccessfulCall(C)", "0")],
        &[("CurrentCall", "1")],
        &[("OutgoingCall(C)", "2"), ("TotalCallCreated", "2")],
        &[("SuccessfulCall(C)", "2")],
        &[("IncomingCall(C)", "1")],
        &[("FailedCall(C)", "1")],
        &[("FailedTimeoutOnRecv(C)", "1")],
        &[("OutOfCallMsgs(C)", "1")],
        &[("Warnings(C)", "1")],
        &[("FatalErrors(C)", "1")],
        &[("WatchdogMajor(C)", "1")],
    ] {
        assert!(validate_sipp_statistics(&statistics(&[row(changes)])).is_err());
    }
}

#[test]
fn every_native_failure_counter_is_checked_in_every_sample() {
    for name in HEADER
        .split_terminator(';')
        .filter(|name| name.starts_with("Failed"))
    {
        let failed = row(&[(name, "1")]);
        assert!(validate_sipp_statistics(&statistics(&[failed, row(&[])])).is_err());
    }
}

#[test]
fn cumulative_counts_cannot_regress_or_hide_an_extra_call() {
    for earlier in [
        row(&[("SuccessfulCall(C)", "2")]),
        row(&[("OutgoingCall(C)", "2"), ("TotalCallCreated", "2")]),
    ] {
        assert!(validate_sipp_statistics(&statistics(&[earlier, row(&[])])).is_err());
    }
}

#[test]
fn statistics_require_unique_complete_native_columns_and_bounded_records() {
    let valid = String::from_utf8_lossy(&statistics(&[row(&[])])).into_owned();
    for invalid in [
        String::new(),
        format!("{HEADER}\n"),
        valid.replace("FailedTimeoutOnRecv(C)", "UnknownColumn"),
        valid.replace("FailedTimeoutOnRecv(C)", "SuccessfulCall(C)"),
        valid.replacen(";CallLengthStDev(C);", ";", 1),
        valid.trim_end().to_owned(),
        format!("{valid}{HEADER}\n"),
        format!("{HEADER}\n{}", row(&[]).repeat(257)),
        "x".repeat(1024 * 1024 + 1),
    ] {
        assert!(validate_sipp_statistics(invalid.as_bytes()).is_err());
    }
    assert!(validate_sipp_statistics(&[0xff]).is_err());
    assert!(validate_sipp_statistics(&statistics(&[row(&[("SuccessfulCall(C)", "-1")])])).is_err());
    assert!(validate_sipp_statistics(&statistics(&[row(&[(
        "SuccessfulCall(C)",
        "18446744073709551616"
    )])]))
    .is_err());
    assert!(
        validate_sipp_statistics(&statistics(&[row(&[("SuccessfulCall(C)", "1.0")])])).is_err()
    );
}

use super::ReferenceError;

#[derive(Clone, Copy)]
enum Counter {
    Zero,
    OutgoingPeriod,
    Outgoing,
    Created,
    Current,
    SuccessfulPeriod,
    Successful,
    RetransmissionsPeriod,
    Retransmissions,
}

#[derive(Clone, Copy)]
enum Field {
    Value,
    Count(Counter),
}

use Counter::{
    Created, Current, Outgoing, OutgoingPeriod, Retransmissions, RetransmissionsPeriod, Successful,
    SuccessfulPeriod, Zero,
};
use Field::{Count, Value};

const COLUMNS: [(&str, Field); 72] = [
    ("StartTime", Value),
    ("LastResetTime", Value),
    ("CurrentTime", Value),
    ("ElapsedTime(P)", Value),
    ("ElapsedTime(C)", Value),
    ("TargetRate", Value),
    ("CallRate(P)", Value),
    ("CallRate(C)", Value),
    ("IncomingCall(P)", Count(Zero)),
    ("IncomingCall(C)", Count(Zero)),
    ("OutgoingCall(P)", Count(OutgoingPeriod)),
    ("OutgoingCall(C)", Count(Outgoing)),
    ("TotalCallCreated", Count(Created)),
    ("CurrentCall", Count(Current)),
    ("SuccessfulCall(P)", Count(SuccessfulPeriod)),
    ("SuccessfulCall(C)", Count(Successful)),
    ("FailedCall(P)", Count(Zero)),
    ("FailedCall(C)", Count(Zero)),
    ("FailedCannotSendMessage(P)", Count(Zero)),
    ("FailedCannotSendMessage(C)", Count(Zero)),
    ("FailedMaxUDPRetrans(P)", Count(Zero)),
    ("FailedMaxUDPRetrans(C)", Count(Zero)),
    ("FailedTcpConnect(P)", Count(Zero)),
    ("FailedTcpConnect(C)", Count(Zero)),
    ("FailedTcpClosed(P)", Count(Zero)),
    ("FailedTcpClosed(C)", Count(Zero)),
    ("FailedUnexpectedMessage(P)", Count(Zero)),
    ("FailedUnexpectedMessage(C)", Count(Zero)),
    ("FailedCallRejected(P)", Count(Zero)),
    ("FailedCallRejected(C)", Count(Zero)),
    ("FailedCmdNotSent(P)", Count(Zero)),
    ("FailedCmdNotSent(C)", Count(Zero)),
    ("FailedRegexpDoesntMatch(P)", Count(Zero)),
    ("FailedRegexpDoesntMatch(C)", Count(Zero)),
    ("FailedRegexpShouldntMatch(P)", Count(Zero)),
    ("FailedRegexpShouldntMatch(C)", Count(Zero)),
    ("FailedRegexpHdrNotFound(P)", Count(Zero)),
    ("FailedRegexpHdrNotFound(C)", Count(Zero)),
    ("FailedOutboundCongestion(P)", Count(Zero)),
    ("FailedOutboundCongestion(C)", Count(Zero)),
    ("FailedTimeoutOnRecv(P)", Count(Zero)),
    ("FailedTimeoutOnRecv(C)", Count(Zero)),
    ("FailedTimeoutOnSend(P)", Count(Zero)),
    ("FailedTimeoutOnSend(C)", Count(Zero)),
    ("FailedTestDoesntMatch(P)", Count(Zero)),
    ("FailedTestDoesntMatch(C)", Count(Zero)),
    ("FailedTestShouldntMatch(P)", Count(Zero)),
    ("FailedTestShouldntMatch(C)", Count(Zero)),
    ("FailedStrcmpDoesntMatch(P)", Count(Zero)),
    ("FailedStrcmpDoesntMatch(C)", Count(Zero)),
    ("FailedStrcmpShouldntMatch(P)", Count(Zero)),
    ("FailedStrcmpShouldntMatch(C)", Count(Zero)),
    ("OutOfCallMsgs(P)", Count(Zero)),
    ("OutOfCallMsgs(C)", Count(Zero)),
    ("DeadCallMsgs(P)", Count(Zero)),
    ("DeadCallMsgs(C)", Count(Zero)),
    ("Retransmissions(P)", Count(RetransmissionsPeriod)),
    ("Retransmissions(C)", Count(Retransmissions)),
    ("AutoAnswered(P)", Count(Zero)),
    ("AutoAnswered(C)", Count(Zero)),
    ("Warnings(P)", Count(Zero)),
    ("Warnings(C)", Count(Zero)),
    ("FatalErrors(P)", Count(Zero)),
    ("FatalErrors(C)", Count(Zero)),
    ("WatchdogMajor(P)", Count(Zero)),
    ("WatchdogMajor(C)", Count(Zero)),
    ("WatchdogMinor(P)", Count(Zero)),
    ("WatchdogMinor(C)", Count(Zero)),
    ("CallLength(P)", Value),
    ("CallLength(C)", Value),
    ("CallLengthStDev(P)", Value),
    ("CallLengthStDev(C)", Value),
];

#[derive(Default)]
struct Sample {
    outgoing: u64,
    current: u64,
    successful: u64,
    retransmissions: u64,
}

fn fields(line: &str) -> Result<impl Iterator<Item = &str>, ReferenceError> {
    line.strip_suffix(';')
        .map(|line| line.split(';'))
        .ok_or(ReferenceError::IncompleteSippStatistics)
}

fn sample(line: &str) -> Result<Sample, ReferenceError> {
    let mut fields = fields(line)?;
    let mut sample = Sample::default();
    let mut outgoing_period = 0;
    let mut successful_period = 0;
    let mut retransmissions_period = 0;
    let mut created = 0;
    for (_, kind) in COLUMNS {
        let value = fields
            .next()
            .filter(|value| {
                !value.is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
            })
            .ok_or(ReferenceError::IncompleteSippStatistics)?;
        match kind {
            Value => {}
            Count(counter) => {
                if !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(ReferenceError::IncompleteSippStatistics);
                }
                let count = value
                    .parse::<u64>()
                    .map_err(|_| ReferenceError::IncompleteSippStatistics)?;
                match counter {
                    Zero => {
                        if count != 0 {
                            return Err(ReferenceError::IncompleteSippStatistics);
                        }
                    }
                    OutgoingPeriod => outgoing_period = count,
                    Outgoing => sample.outgoing = count,
                    Created => created = count,
                    Current => sample.current = count,
                    SuccessfulPeriod => successful_period = count,
                    Successful => sample.successful = count,
                    RetransmissionsPeriod => retransmissions_period = count,
                    Retransmissions => sample.retransmissions = count,
                }
            }
        }
    }
    if fields.next().is_some()
        || created > 1
        || sample.outgoing != created
        || sample.current.checked_add(sample.successful) != Some(created)
        || outgoing_period > sample.outgoing
        || successful_period > sample.successful
        || retransmissions_period > sample.retransmissions
    {
        return Err(ReferenceError::IncompleteSippStatistics);
    }
    Ok(sample)
}

pub fn validate_sipp_statistics(bytes: &[u8]) -> Result<u64, ReferenceError> {
    if bytes.len() > 1024 * 1024 || bytes.last() != Some(&b'\n') {
        return Err(ReferenceError::IncompleteSippStatistics);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| ReferenceError::IncompleteSippStatistics)?;
    let mut lines = text.lines();
    let header = lines
        .next()
        .ok_or(ReferenceError::IncompleteSippStatistics)?;
    if !fields(header)?.eq(COLUMNS.iter().map(|(name, _)| *name)) {
        return Err(ReferenceError::IncompleteSippStatistics);
    }
    let mut previous = Sample::default();
    let mut count = 0;
    for line in lines {
        count += 1;
        if count > 256 {
            return Err(ReferenceError::IncompleteSippStatistics);
        }
        let sample = sample(line)?;
        if sample.outgoing < previous.outgoing
            || sample.successful < previous.successful
            || sample.retransmissions < previous.retransmissions
        {
            return Err(ReferenceError::IncompleteSippStatistics);
        }
        previous = sample;
    }
    if count == 0 || previous.current != 0 || previous.successful != 1 {
        return Err(ReferenceError::IncompleteSippStatistics);
    }
    Ok(previous.successful)
}

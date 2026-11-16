# Endpoint transfer settings

Endpoint sections accept `allow_transfer` and `refer_blind_progress` in `pjsip.conf`.
Both flags default to `yes`, matching [Asterisk 22.9.0](https://github.com/asterisk/asterisk/blob/22.9.0/res/res_pjsip/pjsip_configuration.c#L2152-L2198).
Explicit `yes` and `no` values preserve independent endpoint policy.
Invalid values report the setting's file and line.

The endpoint record retains transfer permission and progress policy with its authentication references and routing context.
The [transfer decision](adr/keep-transfer-progress-independent-of-call-teardown.md) defines the pending signaling and lifetime behavior.
Configuration support doesn't establish working `REFER` handling or transferred audio.

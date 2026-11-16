# Blind transfer through Dial bridges

The successful in-dialog transfer profile passes a shared native and Asterisk 22.9.0 received-audio fixture.
An authenticated Alice calls Bob through a Dial bridge with two channels.
After receiving a `202` response to `REFER`, Alice sends `BYE` and receives `200` before Charlie answers.
The fixture verifies initial Alice/Bob audio, final Bob/Charlie audio, and a terminating `NOTIFY` containing `SIP/2.0 200 OK`.
Each direction must receive six consecutive G.711 μ-law packets carrying the expected signal.

The fixture keeps displayed caller identification separate from authentication identity.
The authenticated endpoint's context selects the target extension despite a different displayed `From` user.
Both implementations deliver final notifications after the transferor ends the `INVITE` usage.
A separate native regression covers a server-generated `BYE` before the target answers.

The wire sequences differ.
Native sends its initial notification and server-generated `BYE` before Charlie answers.
The fixture received neither Asterisk message before Charlie answered.
Native uses `Event: refer;id=3`, while Asterisk uses `Event: refer`.
[RFC 3515 §2.4.6](https://www.rfc-editor.org/rfc/rfc3515.html#section-2.4.6) makes the first Event `id` optional.
[RFC 5589 §5](https://www.rfc-editor.org/rfc/rfc5589.html#section-5) separates successful `REFER` from session termination.
Wire termination timing remains an open compatibility difference.

Run `mise x -- just interop` to execute the named reference case explicitly.
The command rejects missing or skipped execution, restores copied configuration, and removes its isolated container.
Native fixtures also verify task shutdown and reuse of User Datagram Protocol (UDP) ports.

The successful attended transfer profile also passes a shared native and Asterisk received-audio fixture.
The fixture establishes source and consultation calls before an in-dialog `REFER` containing `Replaces`.
The surviving participants exchange known audio while the source and consultation transferor dialogs end.
After confirming termination, the fixture changes both signals and requires fresh matching audio in both directions.
Separate native cases end either surviving participant.
Another case checks known audio and source routes across distinct signaling listeners.

Direct `INVITE` replacement, out-of-dialog `REFER`, authorization for dialogs derived from `sips` sessions, and bridges with three or more channels remain unfinished.
Received-audio comparisons for denied, failed, and cancelled transfers remain open.
The [transfer decision](adr/keep-transfer-progress-independent-of-call-teardown.md) remains proposed.

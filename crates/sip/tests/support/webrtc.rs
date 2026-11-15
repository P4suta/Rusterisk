pub fn offer() -> String {
    format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=browser\r\nt=0 0\r\na=group:BUNDLE 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111 0 8 101\r\nc=IN IP4 0.0.0.0\r\na=mid:0\r\na=ice-ufrag:peer\r\na=ice-pwd:PrivateIcePasswordForPeer22\r\na=fingerprint:sha-256 {}00\r\na=setup:actpass\r\na=rtcp-mux\r\na=rtpmap:111 opus/48000/2\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=sendrecv\r\n", "00:".repeat(31))
}

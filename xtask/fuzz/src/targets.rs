#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Settings,
    Controls,
    Sip,
    Media,
}

pub struct Seed {
    pub name: &'static str,
    pub bytes: &'static [u8],
    pub valid: bool,
}

impl Target {
    pub const ALL: [Self; 4] = [Self::Settings, Self::Controls, Self::Sip, Self::Media];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Controls => "media_controls",
            Self::Sip => "sip",
            Self::Media => "media_packets",
        }
    }

    pub const fn features(self) -> &'static str {
        match self {
            Self::Settings | Self::Controls => "fuzz",
            Self::Sip => "fuzz,signaling",
            Self::Media => "fuzz,media",
        }
    }

    pub fn seeds(self) -> &'static [Seed] {
        match self {
            Self::Settings => &[
                Seed { name: "http", bytes: b"[general]\nbindaddr=127.0.0.1\n", valid: true },
                Seed { name: "ari", bytes: b"[fuzzer]\ntype=user\npassword=fuzz-password\n", valid: true },
                Seed { name: "dialplan", bytes: b"[phone]\nexten => 100,1,Answer()\nsame => n,Stasis(fuzzer)\n", valid: true },
                Seed { name: "websocket", bytes: b"[global]\ncontrol_message_format=json\n", valid: true },
                Seed { name: "directive", bytes: b"#include missing.conf\n", valid: false },
                Seed { name: "line-capacity", bytes: &[b'x'; 4097], valid: false },
                Seed { name: "invalid-utf8", bytes: b"\xff\0", valid: false },
            ],
            Self::Controls => &[
                Seed { name: "answer", bytes: b"ANSWER", valid: true },
                Seed { name: "json-direction", bytes: br#"{"command":"SET_MEDIA_DIRECTION","direction":"both"}"#, valid: true },
                Seed { name: "correlation", bytes: b"MARK_MEDIA correlation", valid: true },
                Seed { name: "flow", bytes: b"MEDIA_XOFF", valid: true },
                Seed { name: "json-flow", bytes: br#"{"event":"MEDIA_XON","channel_id":"fuzzer"}"#, valid: true },
                Seed { name: "dtmf", bytes: b"DTMF_END digit:# channel_id:fuzzer", valid: true },
                Seed { name: "foreign-channel", bytes: br#"{"event":"MEDIA_XON","channel_id":"foreign"}"#, valid: false },
                Seed { name: "command-capacity", bytes: &[b'x'; 129], valid: false },
                Seed { name: "event-capacity", bytes: &[b'x'; 65501], valid: false },
                Seed { name: "invalid-utf8", bytes: b"\xff\0", valid: false },
            ],
            Self::Sip => &[
                Seed { name: "options", bytes: b"OPTIONS sip:fuzzer@localhost SIP/2.0\r\nContent-Length: 0\r\n\r\n", valid: true },
                Seed { name: "keepalive", bytes: b"\r\n\r\n", valid: true },
                Seed { name: "binary-body", bytes: b"OPTIONS sip:fuzzer SIP/2.0\r\nContent-Length: 2\r\n\r\n\0\xff", valid: true },
                Seed { name: "sdp", bytes: b"v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 9000 RTP/AVP 0 101\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=sendonly\r\n", valid: true },
                Seed { name: "duplicate-length", bytes: b"OPTIONS sip:fuzzer SIP/2.0\r\nContent-Length: 0\r\nl: 0\r\n\r\n", valid: false },
                Seed { name: "incomplete", bytes: b"OPTIONS sip:fuzzer SIP/2.0\r\nContent-Length: 9\r\n\r\nshort", valid: false },
                Seed { name: "capacity", bytes: &[b'A'; 16385], valid: false },
            ],
            Self::Media => &[
                Seed { name: "terminate", bytes: b"\0\0\0", valid: true },
                Seed { name: "digit", bytes: b"\x03\0\x01#", valid: true },
                Seed { name: "audio", bytes: b"\x10\0\x04\x01\x02\x03\x04", valid: true },
                Seed { name: "rtp", bytes: b"\x80\0\0\x01\0\0\0\x02\0\0\0\x03\x11\x22", valid: true },
                Seed { name: "telephone-event", bytes: b"\x05\x8a\0\xa0", valid: true },
                Seed { name: "oversized-audio", bytes: b"\x10\xff\xff", valid: false },
                Seed { name: "odd-audio", bytes: b"\x10\0\x03\x01\x02\x03", valid: false },
                Seed { name: "rtp-capacity", bytes: &[0; 2049], valid: false },
            ],
        }
    }
}
pub const MAX_INPUT: usize = 65536;

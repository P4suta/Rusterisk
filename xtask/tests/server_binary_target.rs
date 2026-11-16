use xtask::server_distribution::{check_binary_target_header, ServerTarget};

fn fixture(target: ServerTarget) -> Vec<u8> {
    let mut bytes = vec![0; 256];
    match target {
        ServerTarget::LinuxX86_64 | ServerTarget::LinuxArm64 => {
            bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
            bytes[16..18].copy_from_slice(&3u16.to_le_bytes());
            let machine: u16 = if target == ServerTarget::LinuxX86_64 {
                62
            } else {
                183
            };
            bytes[18..20].copy_from_slice(&machine.to_le_bytes());
            bytes[20..24].copy_from_slice(&1u32.to_le_bytes());
            bytes[24..32].copy_from_slice(&4096u64.to_le_bytes());
            bytes[52..54].copy_from_slice(&64u16.to_le_bytes());
        }
        ServerTarget::MacOsArm64 | ServerTarget::MacOsX86_64 => {
            bytes[..4].copy_from_slice(&0xfeedfacfu32.to_le_bytes());
            let cpu: u32 = if target == ServerTarget::MacOsArm64 {
                0x0100000c
            } else {
                0x01000007
            };
            bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
            bytes[12..16].copy_from_slice(&2u32.to_le_bytes());
        }
        ServerTarget::WindowsX86_64 => {
            bytes[..2].copy_from_slice(b"MZ");
            bytes[60..64].copy_from_slice(&64u32.to_le_bytes());
            bytes[64..68].copy_from_slice(b"PE\0\0");
            bytes[68..70].copy_from_slice(&0x8664u16.to_le_bytes());
            bytes[70..72].copy_from_slice(&1u16.to_le_bytes());
            bytes[84..86].copy_from_slice(&112u16.to_le_bytes());
            bytes[86..88].copy_from_slice(&2u16.to_le_bytes());
            bytes[88..90].copy_from_slice(&0x20bu16.to_le_bytes());
            bytes[104..108].copy_from_slice(&4096u32.to_le_bytes());
            bytes[156..158].copy_from_slice(&3u16.to_le_bytes());
        }
    }
    bytes
}

#[test]
fn candidate_headers_must_match_the_declared_format_and_processor() -> std::io::Result<()> {
    for actual in ServerTarget::ALL {
        let bytes = fixture(actual);
        for expected in ServerTarget::ALL {
            assert_eq!(
                check_binary_target_header(expected, &bytes).is_ok(),
                actual == expected,
                "{actual:?} as {expected:?}"
            );
        }
    }
    let current = std::fs::read(std::env::current_exe()?)?;
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => ServerTarget::MacOsArm64,
        ("macos", "x86_64") => ServerTarget::MacOsX86_64,
        ("linux", "aarch64") => ServerTarget::LinuxArm64,
        ("linux", "x86_64") => ServerTarget::LinuxX86_64,
        ("windows", "x86_64") => ServerTarget::WindowsX86_64,
        _ => return Err(std::io::Error::other("unsupported server test host")),
    };
    check_binary_target_header(target, &current).map_err(std::io::Error::other)
}

#[test]
fn candidate_headers_reject_truncation_wrong_width_shared_libraries_and_unbounded_offsets() {
    for target in ServerTarget::ALL {
        let bytes = fixture(target);
        let minimum = match target {
            ServerTarget::LinuxX86_64 | ServerTarget::LinuxArm64 => 64,
            ServerTarget::MacOsArm64 | ServerTarget::MacOsX86_64 => 32,
            ServerTarget::WindowsX86_64 => 200,
        };
        for length in 0..minimum {
            assert!(
                check_binary_target_header(target, &bytes[..length]).is_err(),
                "{target:?} {length}"
            );
        }
    }
    for (target, offset, replacement) in [
        (ServerTarget::LinuxX86_64, 4, 1),
        (ServerTarget::LinuxX86_64, 5, 2),
        (ServerTarget::LinuxX86_64, 16, 1),
        (ServerTarget::LinuxArm64, 7, 9),
        (ServerTarget::MacOsArm64, 12, 6),
        (ServerTarget::WindowsX86_64, 60, 255),
        (ServerTarget::WindowsX86_64, 84, 0),
        (ServerTarget::WindowsX86_64, 87, 32),
        (ServerTarget::WindowsX86_64, 89, 1),
    ] {
        let mut bytes = fixture(target);
        bytes[offset] = replacement;
        assert!(
            check_binary_target_header(target, &bytes).is_err(),
            "{target:?} {offset}"
        );
    }
    let mut bytes = fixture(ServerTarget::WindowsX86_64);
    bytes[60..64].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(check_binary_target_header(ServerTarget::WindowsX86_64, &bytes).is_err());
}

use super::ServerTarget;

#[derive(Clone, Copy, Debug)]
pub struct BinaryTargetError {
    target: ServerTarget,
}

impl std::fmt::Display for BinaryTargetError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "binary header does not match server target {}",
            self.target.triple()
        )
    }
}

impl std::error::Error for BinaryTargetError {}

fn field<const N: usize>(bytes: &[u8], offset: usize) -> Option<[u8; N]> {
    bytes.get(offset..offset.checked_add(N)?)?.try_into().ok()
}

fn elf(bytes: &[u8], machine: u16) -> bool {
    bytes.len() >= 64
        && bytes.get(..7) == Some(b"\x7fELF\x02\x01\x01")
        && matches!(bytes.get(7), Some(0 | 3))
        && matches!(field::<2>(bytes, 16).map(u16::from_le_bytes), Some(2 | 3))
        && field::<2>(bytes, 18).map(u16::from_le_bytes) == Some(machine)
        && field::<4>(bytes, 20).map(u32::from_le_bytes) == Some(1)
        && field::<8>(bytes, 24)
            .map(u64::from_le_bytes)
            .is_some_and(|entry| entry != 0)
        && field::<2>(bytes, 52).map(u16::from_le_bytes) == Some(64)
}

fn mach_o(bytes: &[u8], processor: u32) -> bool {
    bytes.len() >= 32
        && field::<4>(bytes, 0).map(u32::from_le_bytes) == Some(0xfeedfacf)
        && field::<4>(bytes, 4).map(u32::from_le_bytes) == Some(processor)
        && field::<4>(bytes, 12).map(u32::from_le_bytes) == Some(2)
}

fn pe(bytes: &[u8]) -> Option<bool> {
    if bytes.get(..2) != Some(b"MZ") {
        return Some(false);
    }
    let offset = usize::try_from(u32::from_le_bytes(field(bytes, 60)?)).ok()?;
    if !(64..=1024 * 1024).contains(&offset) || bytes.get(offset..offset + 4) != Some(b"PE\0\0") {
        return Some(false);
    }
    let coff = offset + 4;
    let sections = usize::from(u16::from_le_bytes(field(bytes, coff + 2)?));
    let optional = usize::from(u16::from_le_bytes(field(bytes, coff + 16)?));
    let flags = u16::from_le_bytes(field(bytes, coff + 18)?);
    let image = coff + 20;
    Some(
        u16::from_le_bytes(field(bytes, coff)?) == 0x8664
            && (1..=96).contains(&sections)
            && (112..=4096).contains(&optional)
            && bytes.get(image..image + optional + sections * 40).is_some()
            && flags & 2 != 0
            && flags & (0x2000 | 0x0100 | 0x0080 | 0x8000) == 0
            && u16::from_le_bytes(field(bytes, image)?) == 0x20b
            && u32::from_le_bytes(field(bytes, image + 16)?) != 0
            && u16::from_le_bytes(field(bytes, image + 68)?) == 3,
    )
}

pub fn check_binary_target_header(
    target: ServerTarget,
    bytes: &[u8],
) -> Result<(), BinaryTargetError> {
    let matches = match target {
        ServerTarget::LinuxX86_64 => elf(bytes, 62),
        ServerTarget::LinuxArm64 => elf(bytes, 183),
        ServerTarget::MacOsArm64 => mach_o(bytes, 0x0100000c),
        ServerTarget::MacOsX86_64 => mach_o(bytes, 0x01000007),
        ServerTarget::WindowsX86_64 => pe(bytes) == Some(true),
    };
    if matches {
        Ok(())
    } else {
        Err(BinaryTargetError { target })
    }
}

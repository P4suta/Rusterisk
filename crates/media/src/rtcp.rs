use crate::{MediaError, MAX_PACKET};
use rtcp_types::prelude::{RtcpPacketWriter, RtcpPacketWriterExt};
mod validation;
pub(crate) use validation::inspect_rtcp;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceReport {
    pub ssrc: u32,
    pub timestamp: u32,
    pub packets: u32,
    pub octets: u32,
}

impl SourceReport {
    fn builder(self, ntp: u64) -> rtcp_types::SenderReportBuilder {
        rtcp_types::SenderReport::builder(self.ssrc)
            .ntp_timestamp(ntp)
            .rtp_timestamp(self.timestamp)
            .packet_count(self.packets)
            .octet_count(self.octets)
    }
}

fn failure(error: rtcp_types::RtcpWriteError) -> MediaError {
    MediaError::Control(error.to_string())
}

pub fn sender_reports(
    sources: &[SourceReport],
    ntp: u64,
    cname: &str,
) -> Result<Vec<u8>, MediaError> {
    if sources.is_empty() || sources.len() > 4 || cname.is_empty() || cname.len() > 128 {
        return Err(MediaError::Capacity);
    }
    let description = |source: &SourceReport| {
        rtcp_types::Sdes::builder().add_chunk(
            rtcp_types::SdesChunk::builder(source.ssrc)
                .add_item(rtcp_types::SdesItem::builder(1, cname)),
        )
    };
    let mut size = 0;
    for (index, source) in sources.iter().enumerate() {
        if sources[..index]
            .iter()
            .any(|previous| previous.ssrc == source.ssrc)
        {
            return Err(MediaError::InvalidPacket);
        }
        size += source.builder(ntp).calculate_size().map_err(failure)?;
        size += description(source).calculate_size().map_err(failure)?;
    }
    if size > MAX_PACKET {
        return Err(MediaError::Capacity);
    }
    let mut output = vec![0; size];
    let mut position = 0;
    for source in sources {
        let report = source.builder(ntp);
        let size = report.calculate_size().map_err(failure)?;
        report
            .write_into(&mut output[position..position + size])
            .map_err(failure)?;
        position += size;
    }
    for source in sources {
        let description = description(source);
        let size = description.calculate_size().map_err(failure)?;
        description
            .write_into(&mut output[position..position + size])
            .map_err(failure)?;
        position += size;
    }
    Ok(output)
}

pub fn goodbye_reports(
    sources: &[SourceReport],
    ntp: u64,
    cname: &str,
) -> Result<Vec<u8>, MediaError> {
    let mut output = sender_reports(sources, ntp, cname)?;
    let mut goodbye = rtcp_types::Bye::builder();
    for source in sources {
        goodbye = goodbye.add_source(source.ssrc);
    }
    let position = output.len();
    let size = goodbye.calculate_size().map_err(failure)?;
    if position + size > MAX_PACKET {
        return Err(MediaError::Capacity);
    }
    output.resize(position + size, 0);
    goodbye
        .write_into(&mut output[position..])
        .map_err(failure)?;
    Ok(output)
}

use crate::{Error, Result};
use prost::Message;
use std::io::{Read, Write};

include!(concat!(env!("OUT_DIR"), "/knowcode.rs"));

pub const VERSION: u32 = 7;
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

pub fn read_frame(reader: &mut impl Read) -> Result<Option<Envelope>> {
    let mut header = [0; 4];
    if reader.read(&mut header[..1])? == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..])?;
    let size = u32::from_le_bytes(header) as usize;
    if size == 0 || size > MAX_FRAME {
        return Err(Error::Protocol(format!(
            "frame length {size} exceeds protocol bounds"
        )));
    }
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes)?;
    let envelope = Envelope::decode(bytes.as_slice())?;
    if envelope.protocol_version != VERSION {
        return Err(Error::UpdateRequired(format!(
            "protocol {} is unsupported; expected {VERSION}",
            envelope.protocol_version
        )));
    }
    Ok(Some(envelope))
}

pub fn write_frame(writer: &mut impl Write, envelope: &Envelope) -> Result<()> {
    let bytes = envelope.encode_to_vec();
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(Error::Protocol(
            "outgoing frame exceeds protocol bounds".into(),
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

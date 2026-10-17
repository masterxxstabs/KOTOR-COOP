use serde::{de::DeserializeOwned, Serialize};

/// Largest accepted frame. World snapshots of a long campaign stay far below
/// this; anything bigger is treated as a malicious or corrupted stream.
pub const MAX_FRAME_LEN: usize = 4 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("frame of {0} bytes exceeds the {MAX_FRAME_LEN} byte limit")]
    TooLarge(usize),
    #[error("malformed payload: {0}")]
    Decode(#[from] postcard::Error),
}

/// Serializes `msg` into a length-prefixed frame ready to be written to a socket.
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let mut out = vec![0u8; 4];
    out = postcard::to_extend(msg, out)?;
    let len = out.len() - 4;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::TooLarge(len));
    }
    out[..4].copy_from_slice(&(len as u32).to_le_bytes());
    Ok(out)
}

/// Decodes a payload whose length prefix has already been stripped.
pub fn decode_payload<T: DeserializeOwned>(payload: &[u8]) -> Result<T, FrameError> {
    Ok(postcard::from_bytes(payload)?)
}

/// Incremental decoder for non-blocking readers: feed it whatever bytes arrive
/// and pull out complete messages.
#[derive(Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn extend(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// Returns the next complete message, `Ok(None)` if more bytes are needed.
    pub fn try_next<T: DeserializeOwned>(&mut self) -> Result<Option<T>, FrameError> {
        if self.buf.len() < 4 {
            return Ok(None);
        }
        let len = u32::from_le_bytes(self.buf[..4].try_into().unwrap()) as usize;
        if len > MAX_FRAME_LEN {
            return Err(FrameError::TooLarge(len));
        }
        if self.buf.len() < 4 + len {
            return Ok(None);
        }
        let msg = decode_payload(&self.buf[4..4 + len]);
        self.buf.drain(..4 + len);
        msg.map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::*;

    #[test]
    fn roundtrip_through_decoder_in_pieces() {
        let msgs = vec![
            ClientMessage::Ping { nonce: 7 },
            ClientMessage::PlayerState(PlayerState {
                module: "end_m01aa".into(),
                position: Vec3::new(1.0, 2.0, 3.0),
                facing: 1.5,
                hp: 20,
                max_hp: 24,
                ..Default::default()
            }),
            ClientMessage::SetGlobal {
                name: "K_SWG_HELENA".into(),
                value: GlobalValue::Number(3),
            },
        ];
        let bytes: Vec<u8> = msgs.iter().flat_map(|m| encode_frame(m).unwrap()).collect();

        let mut dec = FrameDecoder::new();
        let mut out = Vec::new();
        for chunk in bytes.chunks(3) {
            dec.extend(chunk);
            while let Some(m) = dec.try_next::<ClientMessage>().unwrap() {
                out.push(m);
            }
        }
        assert_eq!(out, msgs);
    }

    #[test]
    fn rejects_oversized_length_prefix() {
        let mut dec = FrameDecoder::new();
        dec.extend(&(u32::MAX).to_le_bytes());
        assert!(matches!(
            dec.try_next::<ClientMessage>(),
            Err(FrameError::TooLarge(_))
        ));
    }

    #[test]
    fn module_state_dedupes_events() {
        let mut st = ModuleState::default();
        let ev = WorldEvent::CreatureDied {
            module: "m".into(),
            tag: "sith01".into(),
        };
        assert!(st.apply(&ev));
        assert!(!st.apply(&ev));
        let flag = WorldEvent::LocalFlag {
            module: "m".into(),
            key: "k".into(),
            value: 1,
        };
        assert!(st.apply(&flag));
        assert!(!st.apply(&flag));
    }
}

use std::io::{Read, Write};

use anyhow::Context;
use byteorder::{NetworkEndian, ReadBytesExt, WriteBytesExt};
use serde::{Deserialize, Serialize};
use tokio_tungstenite::tungstenite::Message;

pub const OP_HEARTBEAT: u32 = 2;
pub const OP_HEARTBEAT_REPLY: u32 = 3;
pub const OP_MESSAGE: u32 = 5;
pub const OP_USER_AUTHENTICATION: u32 = 7;
pub const OP_CONNECT_SUCCESS: u32 = 8;
pub const BODY_PROTOCOL_VERSION_NORMAL: u16 = 0;
pub const BODY_PROTOCOL_VERSION_BROTLI: u16 = 3;
pub const HEADER_DEFAULT_VERSION: u16 = 1;
pub const HEADER_DEFAULT_SEQUENCE: u32 = 1;

#[derive(Debug, Serialize)]
pub struct AuthMessage {
    pub uid: u32,
    pub roomid: u32,
    pub protover: u16,
    pub platform: &'static str,
    pub r#type: u16,
    pub key: String,
}

#[derive(Debug)]
pub struct Header {
    pub packet_len: u32,
    pub header_len: u16,
    pub proto_ver: u16,
    pub opcode: u32,
    pub seq: u32,
}

impl Header {
    pub fn read_from(reader: &mut impl Read) -> std::io::Result<Header> {
        Ok(Header {
            packet_len: reader.read_u32::<NetworkEndian>()?,
            header_len: reader.read_u16::<NetworkEndian>()?,
            proto_ver: reader.read_u16::<NetworkEndian>()?,
            opcode: reader.read_u32::<NetworkEndian>()?,
            seq: reader.read_u32::<NetworkEndian>()?,
        })
    }

    pub fn write_to(&self, writer: &mut impl Write) -> std::io::Result<()> {
        writer.write_u32::<NetworkEndian>(self.packet_len)?;
        writer.write_u16::<NetworkEndian>(self.header_len)?;
        writer.write_u16::<NetworkEndian>(self.proto_ver)?;
        writer.write_u32::<NetworkEndian>(self.opcode)?;
        writer.write_u32::<NetworkEndian>(self.seq)?;
        Ok(())
    }
}

#[derive(Debug)]
pub enum Body {
    Message(Vec<JsonPacket>),
    HeartbeatReply(i32),
    ConnectSuccess(String),
}

#[derive(Debug)]
pub struct Packet {
    pub header: Header,
    pub body: Body,
}

#[derive(Debug, Deserialize)]
pub struct JsonPacket {
    pub cmd: String,
    pub data: Option<serde_json::Map<String, serde_json::Value>>,
    pub info: Option<Vec<serde_json::Value>>,
    pub msg_self: Option<String>,
}

impl Packet {
    pub fn parse(mut data: &[u8]) -> anyhow::Result<Packet> {
        let header = Header::read_from(&mut data).context("read header")?;
        assert_eq!(
            data.len(),
            header.packet_len as usize - header.header_len as usize
        );

        match header.opcode {
            OP_MESSAGE => {
                let mut msgs = Vec::new();

                match header.proto_ver {
                    BODY_PROTOCOL_VERSION_NORMAL => {
                        log::trace!("Got version 0 message");
                        assert_eq!(
                            header.packet_len as usize - header.header_len as usize,
                            data.len(),
                        );
                        msgs.push(
                            serde_json::from_slice(data).context("decode version 0 message")?,
                        );
                    }
                    BODY_PROTOCOL_VERSION_BROTLI => {
                        let mut data_decomp = Vec::new();
                        brotli::BrotliDecompress(&mut data, &mut data_decomp)
                            .context("brotli decompress")?;
                        let mut data: &[u8] = &data_decomp;

                        while !data.is_empty() {
                            let header = Header::read_from(&mut data).context("read subheader")?;
                            let msg_len = header.packet_len as usize - header.header_len as usize;
                            if msg_len > data.len() {
                                log::error!("Malformed packet: {:?}, {:?}", data_decomp, data);
                                panic!();
                            }
                            let msg_data = &data[..msg_len];
                            msgs.push(serde_json::from_slice(msg_data).context("decode message")?);
                            data = &data[msg_len..];
                        }
                    }
                    other => {
                        anyhow::bail!("unknown protocol version: {}", other);
                    }
                }

                Ok(Packet {
                    header,
                    body: Body::Message(msgs),
                })
            }
            OP_HEARTBEAT_REPLY => {
                assert_eq!(data.len(), std::mem::size_of::<i32>());
                Ok(Packet {
                    header,
                    body: Body::HeartbeatReply(data.read_i32::<NetworkEndian>().unwrap()),
                })
            }
            OP_CONNECT_SUCCESS => Ok(Packet {
                header,
                body: Body::ConnectSuccess(
                    String::from_utf8(data.to_vec()).context("decode message")?,
                ),
            }),
            other => Err(anyhow::anyhow!("unknown opcode {}", other)),
        }
    }
}

pub async fn send<T>(
    ws_stream: &mut tokio_tungstenite::WebSocketStream<T>,
    opcode: u32,
    payload: &[u8],
) -> tokio_tungstenite::tungstenite::Result<()>
where
    T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use futures_util::SinkExt;

    let mut data = Vec::with_capacity(16 + payload.len());
    let header = Header {
        packet_len: (std::mem::size_of::<Header>() + payload.len()) as u32,
        header_len: std::mem::size_of::<Header>() as u16,
        proto_ver: HEADER_DEFAULT_VERSION,
        opcode,
        seq: HEADER_DEFAULT_SEQUENCE,
    };
    header.write_to(&mut data).unwrap();
    data.extend_from_slice(payload);
    ws_stream.send(Message::Binary(data)).await
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub room_id: u32,
    pub server: BililiveServer,
    pub token: String,
    pub notices: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct BililiveServer {
    pub host: String,
    pub wss_port: u16,
}

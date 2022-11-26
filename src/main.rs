use std::time::Duration;

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};
use tokio::time::MissedTickBehavior;
use tokio_tungstenite::tungstenite::Message;

mod config;
mod proto;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::builder()
        .filter_level(log::LevelFilter::Debug)
        .parse_default_env()
        .init();

    let config: config::Config = toml::from_str(
        &tokio::fs::read_to_string("config.toml")
            .await
            .context("read config.toml")?,
    )
    .context("parse config.toml")?;

    let ws_url = format!(
        "wss://{}:{}/sub",
        config.server.host, config.server.wss_port,
    );
    let (mut ws_stream, _) = tokio_tungstenite::connect_async(ws_url)
        .await
        .context("connect to WebSocket")?;

    let authmsg = proto::AuthMessage {
        uid: 0,
        roomid: config.room_id,
        protover: proto::BODY_PROTOCOL_VERSION_BROTLI,
        platform: "web",
        r#type: 2,
        key: config.token.clone(),
    };
    let authmsg = serde_json::to_string(&authmsg).context("serealize auth message")?;
    proto::send(
        &mut ws_stream,
        proto::OP_USER_AUTHENTICATION,
        authmsg.as_bytes(),
    )
    .await
    .context("send auth packet")?;

    const HEARTBEAT_DURATION: Duration = Duration::from_secs(30);
    let mut heartbeat_timeout = tokio::time::interval_at(
        tokio::time::Instant::now() + HEARTBEAT_DURATION,
        HEARTBEAT_DURATION,
    );
    heartbeat_timeout.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            Some(msg) = ws_stream.next() => {
                let msg = msg.context("receive from WebSocket")?;
                match msg {
                    Message::Binary(data) => {
                        log::trace!("Got binary packet: {:?}", data);
                        on_packet(&data, &config)?;
                    }
                    Message::Ping(payload) => {
                        log::debug!("Server sent ping with {}-byte payload", payload.len());
                        ws_stream.send(Message::Pong(payload)).await.context("send pong")?;
                    }
                    Message::Close(_) => {
                        log::error!("WebSocket closed");
                        break;
                    }
                    _ => {
                        anyhow::bail!("unsupported message type: {:?}", msg);
                    }
                }
            }
            _ = heartbeat_timeout.tick() => {
                log::debug!("Send heartbeat");
                proto::send(&mut ws_stream, proto::OP_HEARTBEAT, b"")
                    .await
                    .context("send heartbeat packet")?;
            }
            else => break,
        }
    }

    Ok(())
}

fn on_packet(data: &[u8], config: &config::Config) -> anyhow::Result<()> {
    let packet = proto::Packet::parse(data).context("parse packet")?;
    match packet.body {
        proto::Body::Message(msgs) => {
            for msg in msgs {
                log::debug!("Received message with type {}", msg.cmd);
                match msg.cmd.as_str() {
                    "DANMU_MSG" => {
                        // log::debug!("{:?}", msg.info.as_ref().unwrap());
                        let msg_info = msg.info.as_ref().unwrap();
                        let content = msg_info[1].as_str().unwrap();
                        let user_info = msg_info[2].as_array().unwrap();
                        let nickname = user_info[1].as_str().unwrap();
                        let medal = msg_info[3].as_array().unwrap();
                        let emoticon_info = &msg_info[0].as_array().unwrap()[13];

                        let mut log_msg = String::with_capacity(256);
                        use std::fmt::Write;

                        // User representation
                        // TODO: message bubble
                        if medal.is_empty() {
                            write!(log_msg, "{}", nickname).unwrap();
                        } else {
                            let medal_name = medal[1].as_str().unwrap();
                            let medal_level = medal[0].as_u64().unwrap();
                            write!(log_msg, "{} [{}:{}]", nickname, medal_name, medal_level)
                                .unwrap();
                        }
                        if let Some(emoticon_info) = emoticon_info.as_object() {
                            let url = emoticon_info["url"].as_str().unwrap();
                            let id = emoticon_info["emoticon_unique"].as_str().unwrap();
                            write!(
                                log_msg,
                                " sends an emoticon: {} ({} - {})",
                                content, id, url,
                            )
                            .unwrap();
                        } else {
                            write!(log_msg, " says: {}", content).unwrap();
                        }

                        log::info!("{}", log_msg);
                    }
                    "ONLINE_RANK_COUNT" => {
                        if !config.notices {
                            continue;
                        }
                        let count = msg.data.unwrap()["count"].as_u64().unwrap();
                        log::info!("[N] {} persons online", count);
                    }
                    "WATCHED_CHANGE" => {
                        if !config.notices {
                            continue;
                        }
                        let count = msg.data.as_ref().unwrap()["text_large"].as_str().unwrap();
                        log::info!("[N] Number of watchers: {}", count);
                    }
                    "INTERACT_WORD" => {
                        // log::debug!("{:?}", msg.data);
                        let msg_data = &msg.data.as_ref().unwrap();
                        let nickname = msg_data["uname"].as_str().unwrap();
                        let medal = msg_data["fans_medal"].as_object().unwrap();
                        let medal_name = medal["medal_name"].as_str().unwrap();
                        let medal_level = medal["medal_level"].as_u64().unwrap();

                        log::info!(
                            "{} [{}:{}] entered the live room (message type {})",
                            nickname,
                            medal_name,
                            medal_level,
                            msg_data["msg_type"].as_u64().unwrap(),
                        );
                    }
                    "ENTRY_EFFECT" => {
                        let msg_data = msg.data.as_ref().unwrap();
                        log::info!(
                            "Welcome captain (type {}): {}",
                            msg_data["privilege_type"].as_u64().unwrap(),
                            msg_data["copy_writing_v2"].as_str().unwrap(),
                        );
                    }
                    "NOTICE_MSG" => {
                        log::info!("Notice message: {}", msg.msg_self.unwrap());
                    }
                    "USER_TOAST_MSG" => {
                        log::info!(
                            "Toast message: {}",
                            msg.data.as_ref().unwrap()["toast_msg"].as_str().unwrap(),
                        );
                    }
                    "HOT_RANK_CHANGED_V2" => {
                        if !config.notices {
                            continue;
                        }
                        let msg_data = msg.data.as_ref().unwrap();
                        let rank = msg_data["rank"].as_u64().unwrap();
                        let area = msg_data["area_name"].as_str().unwrap();
                        let desc = msg_data["rank_desc"].as_str().unwrap();
                        let countdown = msg_data["countdown"].as_u64().unwrap();
                        log::info!(
                            "[N] We are at top #{} in {} ({})! Countdown {}",
                            rank,
                            area,
                            desc,
                            countdown,
                        );
                    }
                    "HOT_RANK_CHANGED" => {
                        if !config.notices {
                            continue;
                        }
                        log::info!("[N] Received HOT_RANK_CHANGED v1");
                    }
                    "SEND_GIFT" => {
                        let msg_data = msg.data.as_ref().unwrap();
                        let medal = msg_data["medal_info"].as_object().unwrap();

                        log::info!(
                            "Gift received: {} [{}:{}] {} {} * {}",
                            msg_data["uname"].as_str().unwrap(),
                            medal["medal_name"].as_str().unwrap(),
                            medal["medal_level"].as_u64().unwrap(),
                            msg_data["action"].as_str().unwrap(),
                            msg_data["giftName"].as_str().unwrap(),
                            msg_data["num"].as_u64().unwrap(),
                        );
                    }
                    "WIDGET_BANNER" => {
                        let widgets = msg.data.as_ref().unwrap()["widget_list"]
                            .as_object()
                            .unwrap();
                        for (id, widget) in widgets {
                            let widget = widget.as_object().unwrap();
                            log::info!("Widget banner: {} (#{})", widget["title"], id);
                        }
                    }
                    "STOP_LIVE_ROOM_LIST" => (),
                    other => {
                        log::warn!("Unknown message type {}", other);
                    }
                }
            }
        }
        proto::Body::HeartbeatReply(online_count) => {
            log::debug!("Got heartbeat, count = {}", online_count);
        }
        proto::Body::ConnectSuccess(auth_result) => {
            log::debug!("Successfully connected, server says {}", auth_result);
        }
    }
    Ok(())
}

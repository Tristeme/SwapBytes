use futures::StreamExt;
use libp2p::{
    gossipsub, mdns, identify, request_response,
    noise, swarm::{NetworkBehaviour, SwarmEvent}, tcp, yamux, Multiaddr, PeerId,
    core::upgrade,
    StreamProtocol,
};
use serde::{Deserialize, Serialize};
use tokio::{io::{stdin, AsyncBufReadExt, BufReader, AsyncReadExt, AsyncWriteExt}, select, fs::File};
use std::{error::Error, time::Duration, collections::HashMap};
use clap::Parser;

#[derive(NetworkBehaviour)]
struct MyBehaviour {
    mdns: mdns::tokio::Behaviour,
    gossipsub: gossipsub::Behaviour,
    identify: identify::Behaviour,
    request_response: request_response::cbor::Behaviour<FileRequest, FileResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRequest {
    pub filename: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileResponse {
    pub file_data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub sender: String,
    pub content: String,
    pub message_type: MessageType,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    PublicChat,
    FileProposal,
    DirectMessage { recipient: String },
}

#[derive(Parser, Debug)]
#[clap(name = "P2P File Exchange and Chat")]
struct Opt {
    #[clap(long)]
    port: Option<String>,

    #[clap(long)]
    peer: Option<Multiaddr>,

    #[clap(long)]
    nickname: String,

    #[clap(long)]
    bootstrap: Option<Multiaddr>,
}

async fn handle_input_line(
    line: String,
    swarm: &mut libp2p::Swarm<MyBehaviour>,
    nickname: &str,
    public_topic: &gossipsub::IdentTopic,
    nickname_to_peer_id: &HashMap<String, PeerId>,
) -> Result<(), Box<dyn Error>> {
    let args: Vec<&str> = line.split_whitespace().collect();

    match args.get(0).map(|s| *s) {
        Some("/msg") => {
            if args.len() < 2 {
                println!("Usage: /msg <message>");
                return Ok(());
            }
            let message = ChatMessage {
                sender: nickname.to_string(),
                content: args[1..].join(" "),
                message_type: MessageType::PublicChat,
            };
            let message_bytes = serde_json::to_vec(&message)?;
            swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
        }
        Some("/dm") => {
            if args.len() < 3 {
                println!("Usage: /dm <recipient_nickname> <message>");
                return Ok(());
            }
            let recipient = args[1];
            let message = ChatMessage {
                sender: nickname.to_string(),
                content: args[2..].join(" "),
                message_type: MessageType::DirectMessage { recipient: recipient.to_string() },
            };
            let message_bytes = serde_json::to_vec(&message)?;
            swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
        }
        Some("/propose") => {
            if args.len() < 2 {
                println!("Usage: /propose <filename>");
                return Ok(());
            }
            let message = ChatMessage {
                sender: nickname.to_string(),
                content: format!("I want to share the file: {}", args[1]),
                message_type: MessageType::FileProposal,
            };
            let message_bytes = serde_json::to_vec(&message)?;
            swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
        }
        Some("/get") => {
            if args.len() < 3 {
                println!("Usage: /get <peer_nickname> <filename>");
                return Ok(());
            }
            let peer_nickname = args[1];
            let filename = args[2];
            if let Some(peer_id) = nickname_to_peer_id.get(peer_nickname) {
                swarm.behaviour_mut().request_response.send_request(
                    peer_id,
                    FileRequest { filename: filename.to_string() },
                );
                println!("File request sent to {} for file: {}", peer_nickname, filename);
            } else {
                println!("Unknown peer nickname: {}", peer_nickname);
            }
        }
        _ => {
            println!("Unknown command. Available commands: /msg, /dm, /propose, /get");
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let opt = Opt::parse();
    let nickname = opt.nickname.clone();

    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_behaviour(|key| {
            Ok(MyBehaviour {
                mdns: mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?,
                gossipsub: gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub::Config::default(),
                )?,
                identify: identify::Behaviour::new(identify::Config::new(
                    format!("/ipfs/id/1.0.0/{}", nickname).into(),
                    key.public()
                )),
                request_response: request_response::cbor::Behaviour::new(
                    [(StreamProtocol::new("/file-exchange/1.0.0"), request_response::ProtocolSupport::Full)],
                    request_response::Config::default(),
                ),
            })
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))
        .build();

    let public_topic = gossipsub::IdentTopic::new("public_chat");
    swarm.behaviour_mut().gossipsub.subscribe(&public_topic)?;

    let listen_port = opt.port.unwrap_or("0".to_string());
    let multiaddr = format!("/ip4/0.0.0.0/tcp/{}", listen_port);
    swarm.listen_on(multiaddr.parse()?)?;

    if let Some(addr) = opt.peer {
        let addr_clone = addr.clone();
        swarm.dial(addr)?;
        println!("Dialed {:?}", addr_clone);
    }

    if let Some(bootstrap) = opt.bootstrap {
        let bootstrap_clone = bootstrap.clone();
        swarm.dial(bootstrap)?;
        println!("Dialed bootstrap node {:?}", bootstrap_clone);
    }

    let mut stdin = BufReader::new(stdin()).lines();
    let mut peer_nicknames: HashMap<PeerId, String> = HashMap::new();
    let mut nickname_to_peer_id: HashMap<String, PeerId> = HashMap::new();

    println!("Your nickname is: {}", nickname);
    println!("Available commands: /msg <message>, /dm <recipient> <message>, /propose <filename>, /get <peer_nickname> <filename>");

    loop {
        select! {
            Ok(Some(line)) = stdin.next_line() => {
                handle_input_line(line, &mut swarm, &nickname, &public_topic, &nickname_to_peer_id).await?;
            }
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Listening on {:?}", address);
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Received { peer_id, info })) => {
                    if let Some(nick) = info.protocol_version.strip_prefix("/ipfs/id/1.0.0/") {
                        peer_nicknames.insert(peer_id, nick.to_string());
                        nickname_to_peer_id.insert(nick.to_string(), peer_id);
                        println!("Identified peer {} with nickname {}", peer_id, nick);
                    }
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                    propagation_source: _peer_id,
                    message_id: _id,
                    message,
                })) => {
                    if let Ok(chat_message) = serde_json::from_slice::<ChatMessage>(&message.data) {
                        match chat_message.message_type {
                            MessageType::PublicChat => {
                                println!("Public message from {}: {}", chat_message.sender, chat_message.content);
                            }
                            MessageType::DirectMessage { recipient } => {
                                if recipient == nickname {
                                    println!("DM from {}: {}", chat_message.sender, chat_message.content);
                                }
                            }
                            MessageType::FileProposal => {
                                println!("File proposal from {}: {}", chat_message.sender, chat_message.content);
                            }
                        }
                    }
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::RequestResponse(request_response::Event::Message {
                    peer,
                    message: request_response::Message::Request { request, channel, .. },
                    ..
                })) => {
                    if let Some(sender_nick) = peer_nicknames.get(&peer) {
                        println!("Received file request from {}: {:?}", sender_nick, request);
                    } else {
                        println!("Received file request from unknown peer: {:?}", request);
                    }
                    let filename = &request.filename;
                    let file_bytes = match File::open(filename).await {
                        Ok(mut file) => {
                            let mut buffer = Vec::new();
                            if let Err(e) = file.read_to_end(&mut buffer).await {
                                println!("Error reading file: {:?}", e);
                                vec![]
                            } else {
                                buffer
                            }
                        }
                        Err(e) => {
                            println!("File not found: {}, error: {:?}", filename, e);
                            vec![]
                        },
                    };
                    if let Err(e) = swarm.behaviour_mut().request_response.send_response(channel, FileResponse { file_data: file_bytes }) {
                        println!("Error sending file response: {:?}", e);
                    }
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::RequestResponse(request_response::Event::Message {
                    message: request_response::Message::Response { response, request_id, .. },
                    ..
                })) => {
                    println!("Received file response for request {}", request_id);
                    if response.file_data.is_empty() {
                        println!("File not found on remote peer.");
                    } else {
                        println!("Received file of {} bytes", response.file_data.len());
                        let filename = format!("received_file_{}", request_id);
                        if let Err(e) = File::create(&filename).await?.write_all(&response.file_data).await {
                            println!("Error writing file: {:?}", e);
                        } else {
                            println!("File saved as: {}", filename);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
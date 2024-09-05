use futures::StreamExt;
use libp2p::{
    gossipsub, mdns, identify, request_response,
    noise, swarm::{NetworkBehaviour, SwarmEvent}, tcp, yamux, Multiaddr, PeerId,
    StreamProtocol,
};
use serde::{Deserialize, Serialize};
use tokio::{io::{stdin, AsyncBufReadExt, BufReader, AsyncReadExt, AsyncWriteExt}, select, fs::File};
use std::{error::Error, time::Duration, collections::{HashMap, HashSet}, path::PathBuf};

use clap::Parser;

// Define a custom network behaviour struct
#[derive(NetworkBehaviour)]
struct MyBehaviour {
    // mDNS for local network peer discovery
    mdns: mdns::tokio::Behaviour,
    // gossipsub for message publishing/subscribing (Pub/Sub) model
    gossipsub: gossipsub::Behaviour,
    // identify behaviour to recognize peer nodes
    identify: identify::Behaviour,
    // for handling file request/response
    request_response: request_response::cbor::Behaviour<FileRequest, FileResponse>,
}

// File request struct, used for passing messages when requesting files
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileRequest {
    pub filename: String,
}

// File response struct, used for returning file data when responding to a file request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileResponse {
    pub file_data: Vec<u8>,
    pub original_filename: String,
}

// Chat message struct, includes the sender, message content, and message type
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatMessage {
    pub sender: String,
    pub content: String,
    pub message_type: MessageType,
}

// Define different types of messages, such as public chat, file proposals, direct messages, etc.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MessageType {
    PublicChat,// Public chat message
    FileProposal,// Proposal to share a file
    DirectMessage { recipient: String },// Direct message to a specific recipient
    RoomMessage { room: String },// Message sent to a chat room
    CreateRoom { room: String },// Command to create a new chat room
    JoinRoom { room: String },// Command to join a chat room
    LeaveRoom { room: String },// Command to leave a chat room
    ListRooms,// Command to list all available rooms
}

// Define command-line parameters using clap, used to configure the node
#[derive(Parser, Debug)]
#[clap(name = "P2P File Exchange and Chat")]
struct Opt {
    // Port to listen on
    #[clap(long)]
    port: Option<String>,

    // Address of a peer node to connect to
    #[clap(long)]
    peer: Option<Multiaddr>,

    // User's nickname
    #[clap(long)]
    nickname: String,

    // Address of a bootstrap node (optional)
    #[clap(long)]
    bootstrap: Option<Multiaddr>,
}

// AppState struct to store the application's state, such as nickname and joined rooms
struct AppState {
    // The user's nickname
    nickname: String,
    // Set of available rooms
    rooms: HashSet<String>,
    // Set of rooms the user has joined
    joined_rooms: HashSet<String>,
}

// Function to handle user input commands
async fn handle_input_line(
    line: String,// User input command
    swarm: &mut libp2p::Swarm<MyBehaviour>,// The libp2p Swarm for network interactions
    app_state: &mut AppState,// The application's state
    public_topic: &gossipsub::IdentTopic,// The gossipsub public topic
    nickname_to_peer_id: &HashMap<String, PeerId>,// Mapping from nickname to PeerId
) -> Result<(), Box<dyn Error>> {
    // Split user input into arguments
    let args: Vec<&str> = line.split_whitespace().collect();

    // Handle different command types based on the user input
    match args.get(0).map(|s| *s) {
        // Send a public chat message
        Some("/msg") => {
            if args.len() < 2 {
                println!("Usage: /msg <message>");
                return Ok(());
            }
            let message = ChatMessage {
                sender: app_state.nickname.clone(),
                content: args[1..].join(" "),
                message_type: MessageType::PublicChat,// The message type is public chat
            };
            // Serialize the message into bytes and publish it through gossipsub
            let message_bytes = serde_json::to_vec(&message)?;
            swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
        }
        // Send a direct message (DM)
        Some("/dm") => {
            if args.len() < 3 {
                println!("Usage: /dm <recipient_nickname> <message>");
                return Ok(());
            }
            // Get the recipient's nickname
            let recipient = args[1];
            let message = ChatMessage {
                sender: app_state.nickname.clone(),
                content: args[2..].join(" "),
                message_type: MessageType::DirectMessage { recipient: recipient.to_string() },// Direct message type
            };
            // Publish the direct message
            let message_bytes = serde_json::to_vec(&message)?;
            swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
        }
         // File sharing proposal
        Some("/propose") => {
            if args.len() < 2 {
                println!("Usage: /propose <filename>");
                return Ok(());
            }
            let message = ChatMessage {
                sender: app_state.nickname.clone(),
                content: format!("I want to share the file: {}", args[1]),
                message_type: MessageType::FileProposal, // File proposal message type
            };
            let message_bytes = serde_json::to_vec(&message)?;
            swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
        }
        // get the file with sender's nickname and filename
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
        // create a chat room for peers
        Some("/create_room") => {
            if args.len() < 2 {
                println!("Usage: /create_room <room_name>");
                return Ok(());
            }
            let room_name = args[1];
            app_state.rooms.insert(room_name.to_string());
            let message = ChatMessage {
                sender: app_state.nickname.clone(),
                content: room_name.to_string(),
                message_type: MessageType::CreateRoom { room: room_name.to_string() },
            };
            let message_bytes = serde_json::to_vec(&message)?;
            swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
            println!("Room '{}' created", room_name);
        }
        // join the room
        Some("/join_room") => {
            if args.len() < 2 {
                println!("Usage: /join_room <room_name>");
                return Ok(());
            }
            let room_name = args[1];
            if app_state.rooms.contains(room_name) {
                app_state.joined_rooms.insert(room_name.to_string());
                let message = ChatMessage {
                    sender: app_state.nickname.clone(),
                    content: room_name.to_string(),
                    message_type: MessageType::JoinRoom { room: room_name.to_string() },
                };
                let message_bytes = serde_json::to_vec(&message)?;
                swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
                println!("Joined room '{}'", room_name);
            } else {
                println!("Room '{}' does not exist", room_name);
            }
        }
        // leave chat room
        Some("/leave_room") => {
            if args.len() < 2 {
                println!("Usage: /leave_room <room_name>");
                return Ok(());
            }
            let room_name = args[1];
            if app_state.joined_rooms.remove(room_name) {
                let message = ChatMessage {
                    sender: app_state.nickname.clone(),
                    content: room_name.to_string(),
                    message_type: MessageType::LeaveRoom { room: room_name.to_string() },
                };
                let message_bytes = serde_json::to_vec(&message)?;
                swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
                println!("Left room '{}'", room_name);
            } else {
                println!("You are not in room '{}'", room_name);
            }
        }
        // list all available rooms
        Some("/list_rooms") => {
            let message = ChatMessage {
                sender: app_state.nickname.clone(),
                content: String::new(),
                message_type: MessageType::ListRooms,
            };
            let message_bytes = serde_json::to_vec(&message)?;
            swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
            println!("Available rooms: {:?}", app_state.rooms);
        }
        // send messages in room
        Some("/room") => {
            if args.len() < 3 {
                println!("Usage: /room <room_name> <message>");
                return Ok(());
            }
            let room_name = args[1];
            if app_state.joined_rooms.contains(room_name) {
                let message = ChatMessage {
                    sender: app_state.nickname.clone(),
                    content: args[2..].join(" "),
                    message_type: MessageType::RoomMessage { room: room_name.to_string() },
                };
                let message_bytes = serde_json::to_vec(&message)?;
                swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes)?;
            } else {
                println!("You are not in room '{}'. Join the room first.", room_name);
            }
        }
        _ => {
             // Handling for unknown commands
            println!("Unknown command. Available commands: /msg, /dm, /propose, /get, /create_room, /join_room, /leave_room, /list_rooms, /room");
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Parse command-line parameters
    let opt = Opt::parse();
    let nickname = opt.nickname.clone();

    // Create a new libp2p network instance using SwarmBuilder, defining various network behaviours
    let mut swarm = libp2p::SwarmBuilder::with_new_identity()
        .with_tokio()// Use tokio as the async runtime
        .with_tcp(
            tcp::Config::default(),
            noise::Config::new,// Use Noise protocol for encryption
            yamux::Config::default,// Use Yamux as the multiplexing protocol
        )?
        .with_behaviour(|key| {
            Ok(MyBehaviour {
                // Initialize mDNS for local peer discovery
                mdns: mdns::tokio::Behaviour::new(mdns::Config::default(), key.public().to_peer_id())?,
                // Initialize gossipsub for message publishing/subscribing
                gossipsub: gossipsub::Behaviour::new(
                    gossipsub::MessageAuthenticity::Signed(key.clone()),
                    gossipsub::Config::default(),
                )?,
                // Initialize identify behaviour to recognize peer nodes
                identify: identify::Behaviour::new(identify::Config::new(
                    format!("/ipfs/id/1.0.0/{}", nickname).into(),
                    key.public()
                )),
                // Initialize request-response behaviour for file transfer
                request_response: request_response::cbor::Behaviour::new(
                    [(StreamProtocol::new("/file-exchange/1.0.0"), request_response::ProtocolSupport::Full)],
                    request_response::Config::default(),
                ),
            })
        })?
        .with_swarm_config(|cfg| cfg.with_idle_connection_timeout(Duration::from_secs(60)))// Set idle connection timeout
        .build();

    // Define the public chat topic and subscribe to it
    let public_topic = gossipsub::IdentTopic::new("public_chat");
    swarm.behaviour_mut().gossipsub.subscribe(&public_topic)?;

    // Get the listening port and start listening
    let listen_port = opt.port.unwrap_or("0".to_string());
    let multiaddr = format!("/ip4/0.0.0.0/tcp/{}", listen_port);
    swarm.listen_on(multiaddr.parse()?)?;

    // Dial and connect if a peer address is specified
    if let Some(addr) = opt.peer {
        let addr_clone = addr.clone();
        swarm.dial(addr)?;
        println!("Dialed {:?}", addr_clone);
    }

    // Dial and connect to a bootstrap node if specified
    if let Some(bootstrap) = opt.bootstrap {
        let bootstrap_clone = bootstrap.clone();
        swarm.dial(bootstrap)?;
        println!("Dialed bootstrap node {:?}", bootstrap_clone);
    }

    // Used to read user input commands
    let mut stdin = BufReader::new(stdin()).lines();
    // Store mapping of PeerId to nickname
    let mut peer_nicknames: HashMap<PeerId, String> = HashMap::new();
    // Store mapping from nickname to PeerId
    let mut nickname_to_peer_id: HashMap<String, PeerId> = HashMap::new();

    let mut app_state = AppState {
        nickname: nickname.clone(),
        rooms: HashSet::new(),
        joined_rooms: HashSet::new(),
    };

    // Print available command prompt
    println!("Your nickname is: {}", nickname);
    println!("Available commands: /msg, /dm, /propose, /get, /create_room, /join_room, /leave_room, /list_rooms, /room");

    // Event loop to handle user input and network events
    loop {
        select! {
            // Handle user input
            Ok(Some(line)) = stdin.next_line() => {
                handle_input_line(line, &mut swarm, &mut app_state, &public_topic, &nickname_to_peer_id).await?;
            }
            // Handle network events
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    println!("Listening on {:?}", address);
                }
                // Handle identify events
                SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Received { peer_id, info })) => {
                    if let Some(nick) = info.protocol_version.strip_prefix("/ipfs/id/1.0.0/") {
                        peer_nicknames.insert(peer_id, nick.to_string());
                        nickname_to_peer_id.insert(nick.to_string(), peer_id);
                        println!("Identified peer {} with nickname {}", peer_id, nick);
                    }
                }
                // Handle Gossipsub messages
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
                            MessageType::CreateRoom { room } => {
                                app_state.rooms.insert(room.clone());
                                println!("Room '{}' created by {}", room, chat_message.sender);
                            }
                            MessageType::JoinRoom { room } => {
                                println!("{} joined room '{}'", chat_message.sender, room);
                            }
                            MessageType::LeaveRoom { room } => {
                                println!("{} left room '{}'", chat_message.sender, room);
                            }
                            MessageType::ListRooms => {
                                // Do nothing, as we don't need to update our room list based on others' requests
                            }
                            MessageType::RoomMessage { room } => {
                                if app_state.joined_rooms.contains(&room) {
                                    println!("[{}] {} says: {}", room, chat_message.sender, chat_message.content);
                                }
                            }
                        }
                    }
                }
                // Handle file transfer events
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
                    if let Err(e) = swarm.behaviour_mut().request_response.send_response(channel, FileResponse { 
                        file_data: file_bytes,
                        original_filename: filename.to_string(),
                    }) {
                        println!("Error sending file response: {:?}", e);
                    }
                }
                // Handle file responses
                SwarmEvent::Behaviour(MyBehaviourEvent::RequestResponse(request_response::Event::Message {
                    message: request_response::Message::Response { response, request_id, .. },
                    ..
                })) => {
                    println!("Received file response for request {}", request_id);
                    if response.file_data.is_empty() {
                        println!("File not found on remote peer.");
                    } else {
                        println!("Received file of {} bytes", response.file_data.len());
                        let original_filename = PathBuf::from(&response.original_filename);
                        let file_stem = original_filename.file_stem().unwrap_or_default();
                        let file_extension = original_filename.extension().unwrap_or_default();
                        let new_filename = format!("received_{}.{}", file_stem.to_string_lossy(), file_extension.to_string_lossy());
                        if let Err(e) = File::create(&new_filename).await?.write_all(&response.file_data).await {
                            println!("Error writing file: {:?}", e);
                        } else {
                            println!("File saved as: {}", new_filename);
                        }
                    }
                }
                _ => {}
            }
        }
    }
}
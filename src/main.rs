use futures::StreamExt;
use futures::FutureExt; 
use libp2p::{
    gossipsub, mdns, identify, request_response,
    noise, swarm::{NetworkBehaviour, SwarmEvent}, tcp, yamux, Multiaddr, PeerId,
    StreamProtocol,
};
use serde::{Deserialize, Serialize};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, fs::File};
use std::{error::Error, time::Duration, collections::{HashMap, HashSet}, path::PathBuf};
use clap::Parser;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::{Backend, CrosstermBackend},
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Span, Spans, Text},
    widgets::{Block, Borders, List, ListItem, Paragraph},
    Frame, Terminal,
};
use libp2p::gossipsub::ConfigBuilder;
use std::time::Instant;
use ratatui::widgets::ListState;

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
    PublicChat,             // Public chat message
    FileProposal,           // Proposal to share a file
    DirectMessage { recipient: String }, // Direct message to a specific recipient
    RoomMessage { room: String },  // Message sent to a chat room
    CreateRoom { room: String },   // Command to create a new chat room
    JoinRoom { room: String },     // Command to join a chat room
    LeaveRoom { room: String },    // Command to leave a chat room
    ListRooms,              // Command to list all available rooms
}

// Define command-line parameters using clap, used to configure the node
#[derive(Parser, Debug)]
#[clap(name = "P2P File Exchange and Chat")]
struct Opt {
    // Port to listen on
    #[clap(long)]
    port: Option<String>,

    // Address of peer nodes to connect to (can be multiple)
    #[clap(long)]
    peer: Option<Vec<Multiaddr>>,

    // User's nickname
    #[clap(long)]
    nickname: String,

    // Address of a bootstrap node (optional)
    #[clap(long)]
    bootstrap: Option<Multiaddr>,
}

// Application state for storing nickname and rooms joined by the user
struct AppState {
    // The user's nickname
    nickname: String,
    // Set of available rooms
    rooms: HashSet<String>,
    // Set of rooms the user has joined
    joined_rooms: HashSet<String>,
}

// Struct to manage application state, including input and message list
struct App {
    input: String,
    messages: Vec<String>,
    messages_state: ListState,
    app_state: AppState,
}

// Implementation of App struct with methods for scrolling and adding messages
impl App {
    fn new(nickname: String) -> App {
        App {
            input: String::new(),
            messages: Vec::new(),
            messages_state: ListState::default(),
            app_state: AppState {
                nickname,
                rooms: HashSet::new(),
                joined_rooms: HashSet::new(),
            },
        }
    }
    
    // Scroll through the message list
    fn scroll_messages(&mut self, scroll: i32) {
        let new_index = match self.messages_state.selected() {
            Some(i) => {
                let i = i as i32;
                let new_index = i + scroll;
                if new_index < 0 {
                    0
                } else if new_index >= self.messages.len() as i32 {
                    self.messages.len() - 1
                } else {
                    new_index as usize
                }
            }
            None => 0,
        };
        self.messages_state.select(Some(new_index));
    }

    // Add a new message to the message list
    fn add_message(&mut self, message: String) {
        self.messages.push(message);
        // Automatically scroll to the newest message
        self.messages_state.select(Some(self.messages.len() - 1));
    }
}

// Function to handle user input and send messages
async fn handle_input_line(
    app: &mut App,
    swarm: &mut libp2p::Swarm<MyBehaviour>,
    public_topic: &gossipsub::IdentTopic,
    nickname_to_peer_id: &HashMap<String, PeerId>,
) -> Result<(), Box<dyn Error>> {
    let line = app.input.clone();
    app.input.clear();
    let args: Vec<&str> = line.split_whitespace().collect();

    match args.get(0).map(|s| *s) {
        // Handle public chat message
        Some("/msg") => {
            if args.len() < 2 {
                app.add_message("Usage: /msg <message>".to_string());
                return Ok(());
            }
            let message = ChatMessage {
                sender: app.app_state.nickname.clone(),
                content: args[1..].join(" "),
                message_type: MessageType::PublicChat,
            };
            let message_bytes = serde_json::to_vec(&message)?;
            match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                Ok(_) => app.add_message("Public message sent".to_string()),
                Err(gossipsub::PublishError::InsufficientPeers) => {
                    app.add_message("Warning: Insufficient peers to propagate the message".to_string());
                }
                Err(e) => app.add_message(format!("Error sending message: {:?}", e)),
            }
        }

        // Handle direct message
        Some("/dm") => {
            if args.len() < 3 {
                app.add_message("Usage: /dm <recipient> <message>".to_string());
                return Ok(());
            }
            let recipient = args[1];
            let message = ChatMessage {
                sender: app.app_state.nickname.clone(),
                content: args[2..].join(" "),
                message_type: MessageType::DirectMessage { recipient: recipient.to_string() },
            };
            let message_bytes = serde_json::to_vec(&message)?;
            match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                Ok(_) => app.add_message(format!("Direct message sent to {}", recipient)),
                Err(gossipsub::PublishError::InsufficientPeers) => {
                    app.add_message("Warning: Insufficient peers to propagate the message".to_string());
                }
                Err(e) => app.add_message(format!("Error sending message: {:?}", e)),
            }
        }

        // Handle file proposal
        Some("/propose") => {
            if args.len() < 2 {
                app.add_message("Usage: /propose <filename>".to_string());
                return Ok(());
            }
            let message = ChatMessage {
                sender: app.app_state.nickname.clone(),
                content: format!("I want to share file: {}", args[1]),
                message_type: MessageType::FileProposal,
            };
            let message_bytes = serde_json::to_vec(&message)?;
            match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                Ok(_) => app.add_message("File proposal sent".to_string()),
                Err(gossipsub::PublishError::InsufficientPeers) => {
                    app.add_message("Warning: Insufficient peers to propagate the message".to_string());
                }
                Err(e) => app.add_message(format!("Error sending message: {:?}", e)),
            }
        }

        // Handle file request
        Some("/get") => {
            if args.len() < 3 {
                app.add_message("Usage: /get <peer_nickname> <filename>".to_string());
                return Ok(());
            }
            let peer_nickname = args[1];
            let filename = args[2];
            app.add_message(format!("Current nickname_to_peer_id mapping: {:?}", nickname_to_peer_id));

            if let Some(peer_id) = nickname_to_peer_id.get(peer_nickname) {
                swarm.behaviour_mut().request_response.send_request(
                    peer_id,
                    FileRequest { filename: filename.to_string() },
                );
                app.add_message(format!("File request sent to {}: {}", peer_nickname, filename));
            } else {
                app.add_message(format!("Unknown peer nickname: {}", peer_nickname));
            }
        }

        // Handle creating a new room
        Some("/create_room") => {
            if args.len() < 2 {
                app.add_message("Usage: /create_room <room_name>".to_string());
                return Ok(());
            }
            let room_name = args[1];
            app.app_state.rooms.insert(room_name.to_string());
            let message = ChatMessage {
                sender: app.app_state.nickname.clone(),
                content: room_name.to_string(),
                message_type: MessageType::CreateRoom { room: room_name.to_string() },
            };
            let message_bytes = serde_json::to_vec(&message)?;
            match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                Ok(_) => app.add_message(format!("Room '{}' created", room_name)),
                Err(gossipsub::PublishError::InsufficientPeers) => {
                    app.add_message("Warning: Insufficient peers to propagate the message".to_string());
                }
                Err(e) => app.add_message(format!("Error sending message: {:?}", e)),
            }
        }

        // Handle joining a room
        Some("/join_room") => {
            if args.len() < 2 {
                app.add_message("Usage: /join_room <room_name>".to_string());
                return Ok(());
            }
            let room_name = args[1];
            if app.app_state.rooms.contains(room_name) {
                app.app_state.joined_rooms.insert(room_name.to_string());
                let message = ChatMessage {
                    sender: app.app_state.nickname.clone(),
                    content: room_name.to_string(),
                    message_type: MessageType::JoinRoom { room: room_name.to_string() },
                };
                let message_bytes = serde_json::to_vec(&message)?;
                match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                    Ok(_) => app.add_message(format!("Joined room '{}'", room_name)),
                    Err(gossipsub::PublishError::InsufficientPeers) => {
                        app.add_message("Warning: Insufficient peers to propagate the message".to_string());
                    }
                    Err(e) => app.add_message(format!("Error sending message: {:?}", e)),
                }
            } else {
                app.add_message(format!("Room '{}' does not exist", room_name));
            }
        }

        // Handle leaving a room
        Some("/leave_room") => {
            if args.len() < 2 {
                app.add_message("Usage: /leave_room <room_name>".to_string());
                return Ok(());
            }
            let room_name = args[1];
            if app.app_state.joined_rooms.remove(room_name) {
                let message = ChatMessage {
                    sender: app.app_state.nickname.clone(),
                    content: room_name.to_string(),
                    message_type: MessageType::LeaveRoom { room: room_name.to_string() },
                };
                let message_bytes = serde_json::to_vec(&message)?;
                match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                    Ok(_) => app.add_message(format!("Left room '{}'", room_name)),
                    Err(gossipsub::PublishError::InsufficientPeers) => {
                        app.add_message("Warning: Insufficient peers to propagate the message".to_string());
                    }
                    Err(e) => app.add_message(format!("Error sending message: {:?}", e)),
                }
            } else {
                app.add_message(format!("You are not in room '{}'", room_name));
            }
        }

        // Handle listing all rooms
        Some("/list_rooms") => {
            let rooms_list = app.app_state.rooms.iter().cloned().collect::<Vec<String>>().join(", ");
            app.add_message(format!("Available rooms: {}", rooms_list));
        }

        // Handle sending a message to a specific room
        Some("/room") => {
            if args.len() < 3 {
                app.add_message("Usage: /room <room_name> <message>".to_string());
                return Ok(());
            }
            let room_name = args[1];
            if app.app_state.joined_rooms.contains(room_name) {
                let message = ChatMessage {
                    sender: app.app_state.nickname.clone(),
                    content: args[2..].join(" "),
                    message_type: MessageType::RoomMessage { room: room_name.to_string() },
                };
                let message_bytes = serde_json::to_vec(&message)?;
                match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                    Ok(_) => app.add_message(format!("Message sent to room '{}'", room_name)),
                    Err(gossipsub::PublishError::InsufficientPeers) => {
                        app.add_message("Warning: Insufficient peers to propagate the message".to_string());
                    }
                    Err(e) => app.add_message(format!("Error sending message: {:?}", e)),
                }
            } else {
                app.add_message(format!("You are not in room '{}'. Please join the room first.", room_name));
            }
        }

        // Handle unknown commands
        _ => {
            app.add_message("Unknown command. Available commands: /msg, /dm, /propose, /get, /create_room, /join_room, /leave_room, /list_rooms, /room".to_string());
        }
    }

    Ok(())
}

// Function to render the TUI interface
fn ui<B: Backend>(f: &mut Frame<B>, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(2)
        .constraints(
            [
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(3),
            ]
            .as_ref(),
        )
        .split(f.size());

    let (msg, style) = (
        vec![
            Span::raw("Press "),
            Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to exit, "),
            Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to send message, "),
            Span::styled("↑↓", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" to scroll messages."),
        ],
        Style::default().add_modifier(Modifier::RAPID_BLINK),
    );
    let mut text = Text::from(Spans::from(msg));
    text.patch_style(style);
    let help_message = Paragraph::new(text);
    f.render_widget(help_message, chunks[0]);

    let messages: Vec<ListItem> = app
        .messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let content = vec![Spans::from(Span::raw(format!("{}: {}", i, m)))];
            ListItem::new(content)
        })
        .collect();
    let messages = List::new(messages)
        .block(Block::default().borders(Borders::ALL).title("Messages"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(messages, chunks[1], &mut app.messages_state);

    let input = Paragraph::new(app.input.as_ref())
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title("Input"));
    f.render_widget(input, chunks[2]);
    f.set_cursor(
        chunks[2].x + app.input.len() as u16 + 1,
        chunks[2].y + 1,
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // Parse command-line parameters
    let opt = Opt::parse();
    let nickname = opt.nickname.clone();

    // Gossipsub configuration with custom parameters
    let gossipsub_config = ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(1)) // More frequent heartbeats
        .validation_mode(gossipsub::ValidationMode::Permissive) // More lenient message validation
        .mesh_n_low(2)    // Should be greater than 0
        .mesh_n(4)        // Should be greater than mesh_n_low
        .mesh_n_high(8)   // Should be greater than mesh_n
        .gossip_lazy(3)   // Should be less than mesh_n
        .history_length(5)
        .history_gossip(3)
        .fanout_ttl(Duration::from_secs(60))
        .build()
        .expect("Valid config");

    // Build the swarm with TCP, noise, and yamux protocols
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
                    gossipsub_config,
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

    // Define the public chat topic and subscribe to it
    let public_topic = gossipsub::IdentTopic::new("public_chat");
    swarm.behaviour_mut().gossipsub.subscribe(&public_topic)?;

    // Get the listening port and start listening
    let listen_port = opt.port.unwrap_or("0".to_string());
    let multiaddr = format!("/ip4/0.0.0.0/tcp/{}", listen_port);
    swarm.listen_on(multiaddr.parse()?)?;

    // Create the application state
    let mut app = App::new(nickname);
    
    // If peer nodes are specified, dial and connect to them
    if let Some(peers) = &opt.peer {
        for addr in peers {
            swarm.dial(addr.clone())?;
            app.add_message(format!("Dialing peer: {:?}", addr));
        }
    }

    // If a bootstrap node is specified, dial and connect to it
    if let Some(bootstrap) = opt.bootstrap {
        swarm.dial(bootstrap)?;
    }

    // Set up terminal for TUI
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // HashMaps to store mappings between PeerId and nicknames
    let mut peer_nicknames: HashMap<PeerId, String> = HashMap::new();
    let mut nickname_to_peer_id: HashMap<String, PeerId> = HashMap::new();

    let mut last_status_update = Instant::now();

    // Main event loop to handle user input and network events
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        // Poll for user input
        if event::poll(Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                match key.code {
                    KeyCode::Esc => {
                        break;
                    }
                    KeyCode::Enter => {
                        handle_input_line(&mut app, &mut swarm, &public_topic, &nickname_to_peer_id).await?;
                    }
                    KeyCode::Char(c) => {
                        app.input.push(c);
                    }
                    KeyCode::Backspace => {
                        app.input.pop();
                    }
                    KeyCode::Up => {
                        app.scroll_messages(-1);
                    }
                    KeyCode::Down => {
                        app.scroll_messages(1);
                    }
                    _ => {}
                }
            }
        }

        // Handle network events
        while let Some(event) = swarm.select_next_some().now_or_never() {
            match event {
                SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Discovered(peers))) => {
                    for (peer_id, addr) in peers {
                        app.add_message(format!("Discovered peer: {:?} at {:?}", peer_id, addr));
                
                        // 尝试连接发现的 peer
                        if let Err(e) = swarm.dial(addr.clone()) {
                            app.add_message(format!("Failed to dial discovered peer {:?}: {:?}", peer_id, e));
                        } else {
                            app.add_message(format!("Dialing discovered peer: {:?}", peer_id));
                        }
                    }
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::Mdns(mdns::Event::Expired(peers))) => {
                    for (peer_id, _addr) in peers {
                        app.add_message(format!("Expired peer: {:?}", peer_id));
                    }
                }
                // Handle new listening address
                SwarmEvent::NewListenAddr { address, .. } => {
                    app.add_message(format!("Listening on {:?}", address));
                }
                // Handle peer identification
                SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Received { peer_id, info })) => {
                    if let Some(nick) = info.protocol_version.strip_prefix("/ipfs/id/1.0.0/") {
                        peer_nicknames.insert(peer_id, nick.to_string());
                        nickname_to_peer_id.insert(nick.to_string(), peer_id);
                        app.add_message(format!("Identified peer {} with nickname {}", peer_id, nick));
                        app.add_message(format!("Nickname to PeerId mapping updated: {:?} -> {:?}", nick, peer_id));
                    }
                }
                // Handle subscription to topics
                SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Subscribed { peer_id, topic })) => {
                    app.add_message(format!("Subscribed to topic {:?} from peer {:?}", topic, peer_id));
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
                                app.add_message(format!("Public message from {}: {}", chat_message.sender, chat_message.content));
                            }
                            MessageType::DirectMessage { recipient } => {
                                if recipient == app.app_state.nickname {
                                    app.add_message(format!("Direct message from {}: {}", chat_message.sender, chat_message.content));
                                }
                            }
                            MessageType::FileProposal => {
                                app.add_message(format!("File proposal from {}: {}", chat_message.sender, chat_message.content));
                            }
                            MessageType::CreateRoom { room } => {
                                app.app_state.rooms.insert(room.clone());
                                app.add_message(format!("Room '{}' created by {}", room, chat_message.sender));
                            }
                            MessageType::JoinRoom { room } => {
                                app.add_message(format!("{} joined room '{}'", chat_message.sender, room));
                            }
                            MessageType::LeaveRoom { room } => {
                                app.add_message(format!("{} left room '{}'", chat_message.sender, room));
                            }
                            MessageType::RoomMessage { room } => {
                                if app.app_state.joined_rooms.contains(&room) {
                                    app.add_message(format!("[{}] {} says: {}", room, chat_message.sender, chat_message.content));
                                }
                            }
                            _ => {}
                        }
                    }
                }
                // Handle file requests
                SwarmEvent::Behaviour(MyBehaviourEvent::RequestResponse(request_response::Event::Message {
                    peer,
                    message: request_response::Message::Request { request, channel, .. },
                    ..
                })) => {
                    if let Some(sender_nick) = peer_nicknames.get(&peer) {
                        app.add_message(format!("Received file request from {}: {:?}", sender_nick, request));
                    } else {
                        app.add_message(format!("Received file request from unknown peer: {:?}", request));
                    }
                    let filename = &request.filename;
                    let file_bytes = match File::open(filename).await {
                        Ok(mut file) => {
                            let mut buffer = Vec::new();
                            if let Err(e) = file.read_to_end(&mut buffer).await {
                                app.add_message(format!("Error reading file: {:?}", e));
                                vec![]
                            } else {
                                buffer
                            }
                        }
                        Err(e) => {
                            app.add_message(format!("File not found: {}, error: {:?}", filename, e));
                            vec![]
                        },
                    };
                    if let Err(e) = swarm.behaviour_mut().request_response.send_response(channel, FileResponse { 
                        file_data: file_bytes,
                        original_filename: filename.to_string(),
                    }) {
                        app.add_message(format!("Error sending file response: {:?}", e));
                    }
                }
                // Handle file responses
                SwarmEvent::Behaviour(MyBehaviourEvent::RequestResponse(request_response::Event::Message {
                    message: request_response::Message::Response { response, request_id, .. },
                    ..
                })) => {
                    app.add_message(format!("Received file response for request {}", request_id));
                    if response.file_data.is_empty() {
                        app.add_message("File not found on remote peer.".to_string());
                    } else {
                        app.add_message(format!("Received file of {} bytes", response.file_data.len()));
                        let original_filename = PathBuf::from(&response.original_filename);
                        let file_stem = original_filename.file_stem().unwrap_or_default();
                        let file_extension = original_filename.extension().unwrap_or_default();
                        let new_filename = format!("received_{}.{}", file_stem.to_string_lossy(), file_extension.to_string_lossy());
                        if let Err(e) = File::create(&new_filename).await?.write_all(&response.file_data).await {
                            app.add_message(format!("Error writing file: {:?}", e));
                        } else {
                            app.add_message(format!("File saved as: {}", new_filename));
                        }
                    }
                }
                // Handle connection established
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    app.add_message(format!("Connected to peer {:?}", peer_id));
                }
                // Handle connection closed
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    app.add_message(format!("Disconnected from peer {:?}", peer_id));
                }
                // Handle connection errors
                SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                    app.add_message(format!("Error connecting to peer {:?}: {:?}", peer_id, error));
                }
                SwarmEvent::IncomingConnectionError { send_back_addr, error, .. } => {
                    app.add_message(format!("Error with incoming connection from {:?}: {:?}", send_back_addr, error));
                }
                SwarmEvent::Dialing { peer_id, connection_id: _ } => {
                    app.add_message(format!("Dialing peer {:?}", peer_id));
                }
                _ => {}
            }
        }
        // Periodically check and display connection status
        // if last_status_update.elapsed() > Duration::from_secs(30) {
        //     let connected_peers = swarm.connected_peers().count();
        //     app.add_message(format!("Number of connected peers: {}", connected_peers));
        //     app.add_message(format!("Known peers: {:?}", peer_nicknames.keys().collect::<Vec<_>>()));
        //     last_status_update = Instant::now();
        // }
    }

    // Restore the terminal state before exiting
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

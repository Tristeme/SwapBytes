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

// 新的结构体用于保存应用程序状态
struct App {
    input: String,
    messages: Vec<String>,
    messages_state: ListState,
    app_state: AppState,
}

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
    // 添加新的方法来滚动消息
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
    fn add_message(&mut self, message: String) {
        self.messages.push(message);
        // 自动滚动到最新消息
        self.messages_state.select(Some(self.messages.len() - 1));
    }
}

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
        Some("/msg") => {
            if args.len() < 2 {
                app.add_message("用法: /msg <消息>".to_string());
                return Ok(());
            }
            let message = ChatMessage {
                sender: app.app_state.nickname.clone(),
                content: args[1..].join(" "),
                message_type: MessageType::PublicChat,
            };
            let message_bytes = serde_json::to_vec(&message)?;
            match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                Ok(_) => app.add_message("公共消息已发送".to_string()),
                Err(gossipsub::PublishError::InsufficientPeers) => {
                    app.add_message("警告: 没有足够的对等节点来传播消息".to_string());
                }
                Err(e) => app.add_message(format!("发送消息时出错: {:?}", e)),
            }
        }
        Some("/dm") => {
            if args.len() < 3 {
                app.add_message("用法: /dm <接收者昵称> <消息>".to_string());
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
                Ok(_) => app.add_message(format!("私信已发送给 {}", recipient)),
                Err(gossipsub::PublishError::InsufficientPeers) => {
                    app.add_message("警告: 没有足够的对等节点来传播消息".to_string());
                }
                Err(e) => app.add_message(format!("发送消息时出错: {:?}", e)),
            }
        }
        Some("/propose") => {
            if args.len() < 2 {
                app.add_message("用法: /propose <文件名>".to_string());
                return Ok(());
            }
            let message = ChatMessage {
                sender: app.app_state.nickname.clone(),
                content: format!("我想分享文件: {}", args[1]),
                message_type: MessageType::FileProposal,
            };
            let message_bytes = serde_json::to_vec(&message)?;
            match swarm.behaviour_mut().gossipsub.publish(public_topic.clone(), message_bytes) {
                Ok(_) => app.add_message("文件分享提议已发送".to_string()),
                Err(gossipsub::PublishError::InsufficientPeers) => {
                    app.add_message("警告: 没有足够的对等节点来传播消息".to_string());
                }
                Err(e) => app.add_message(format!("发送消息时出错: {:?}", e)),
            }
        }
        Some("/get") => {
            if args.len() < 3 {
                app.add_message("用法: /get <对等节点昵称> <文件名>".to_string());
                return Ok(());
            }
            let peer_nickname = args[1];
            let filename = args[2];
            if let Some(peer_id) = nickname_to_peer_id.get(peer_nickname) {
                swarm.behaviour_mut().request_response.send_request(
                    peer_id,
                    FileRequest { filename: filename.to_string() },
                );
                app.add_message(format!("已向 {} 发送文件请求: {}", peer_nickname, filename));
            } else {
                app.add_message(format!("未知的对等节点昵称: {}", peer_nickname));
            }
        }
        Some("/create_room") => {
            if args.len() < 2 {
                app.add_message("用法: /create_room <房间名>".to_string());
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
                Ok(_) => app.add_message(format!("房间 '{}' 已创建", room_name)),
                Err(gossipsub::PublishError::InsufficientPeers) => {
                    app.add_message("警告: 没有足够的对等节点来传播消息".to_string());
                }
                Err(e) => app.add_message(format!("发送消息时出错: {:?}", e)),
            }
        }
        Some("/join_room") => {
            if args.len() < 2 {
                app.add_message("用法: /join_room <房间名>".to_string());
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
                    Ok(_) => app.add_message(format!("已加入房间 '{}'", room_name)),
                    Err(gossipsub::PublishError::InsufficientPeers) => {
                        app.add_message("警告: 没有足够的对等节点来传播消息".to_string());
                    }
                    Err(e) => app.add_message(format!("发送消息时出错: {:?}", e)),
                }
            } else {
                app.add_message(format!("房间 '{}' 不存在", room_name));
            }
        }
        Some("/leave_room") => {
            if args.len() < 2 {
                app.add_message("用法: /leave_room <房间名>".to_string());
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
                    Ok(_) => app.add_message(format!("已离开房间 '{}'", room_name)),
                    Err(gossipsub::PublishError::InsufficientPeers) => {
                        app.add_message("警告: 没有足够的对等节点来传播消息".to_string());
                    }
                    Err(e) => app.add_message(format!("发送消息时出错: {:?}", e)),
                }
            } else {
                app.add_message(format!("你不在房间 '{}' 中", room_name));
            }
        }
        Some("/list_rooms") => {
            let rooms_list = app.app_state.rooms.iter().cloned().collect::<Vec<String>>().join(", ");
            app.add_message(format!("可用房间: {}", rooms_list));
        }
        Some("/room") => {
            if args.len() < 3 {
                app.add_message("用法: /room <房间名> <消息>".to_string());
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
                    Ok(_) => app.add_message(format!("消息已发送到房间 '{}'", room_name)),
                    Err(gossipsub::PublishError::InsufficientPeers) => {
                        app.add_message("警告: 没有足够的对等节点来传播消息".to_string());
                    }
                    Err(e) => app.add_message(format!("发送消息时出错: {:?}", e)),
                }
            } else {
                app.add_message(format!("你不在房间 '{}' 中。请先加入房间。", room_name));
            }
        }
        _ => {
            app.add_message("未知命令。可用命令: /msg, /dm, /propose, /get, /create_room, /join_room, /leave_room, /list_rooms, /room".to_string());
        }
    }

    Ok(())
}

// 绘制TUI的函数
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
            Span::raw("按 "),
            Span::styled("Esc", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" 退出, "),
            Span::styled("Enter", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" 发送消息, "),
            Span::styled("↑↓", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" 滚动消息。"),
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
        .block(Block::default().borders(Borders::ALL).title("消息"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");
    f.render_stateful_widget(messages, chunks[1], &mut app.messages_state);

    let input = Paragraph::new(app.input.as_ref())
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title("输入"));
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

    let gossipsub_config = ConfigBuilder::default()
        .heartbeat_interval(Duration::from_secs(1)) // 更频繁的心跳
        .validation_mode(gossipsub::ValidationMode::Permissive) // 更宽松的消息验证
        .mesh_n_low(2)        // 最小应该大于0
        .mesh_n(4)            // 应该大于 mesh_n_low
        .mesh_n_high(8)       // 应该大于 mesh_n
        .gossip_lazy(3)       // 应该小于 mesh_n
        .history_length(5)
        .history_gossip(3)
        .fanout_ttl(Duration::from_secs(60))
        .build()
        .expect("Valid config");

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

    // 如果指定了对等节点地址，则拨号并连接
    if let Some(addr) = opt.peer {
        swarm.dial(addr)?;
    }

    // 如果指定了引导节点，则拨号并连接
    if let Some(bootstrap) = opt.bootstrap {
        swarm.dial(bootstrap)?;
    }

    // 设置终端
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // 创建App
    let mut app = App::new(nickname);
    // 存储 PeerId 到昵称的映射
    let mut peer_nicknames: HashMap<PeerId, String> = HashMap::new();
    // 存储昵称到 PeerId 的映射
    let mut nickname_to_peer_id: HashMap<String, PeerId> = HashMap::new();


    let mut last_status_update = Instant::now();

    // Event loop to handle user input and network events
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

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

        // 处理网络事件
        while let Some(event) = swarm.select_next_some().now_or_never() {
            match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    app.add_message(format!("Listening on {:?}", address));
                }
                // Handle identify events
                SwarmEvent::Behaviour(MyBehaviourEvent::Identify(identify::Event::Received { peer_id, info })) => {
                    if let Some(nick) = info.protocol_version.strip_prefix("/ipfs/id/1.0.0/") {
                        peer_nicknames.insert(peer_id, nick.to_string());
                        nickname_to_peer_id.insert(nick.to_string(), peer_id);
                        app.add_message(format!("Identified peer {} with nickname {}", peer_id, nick));
                    }
                }
                SwarmEvent::Behaviour(MyBehaviourEvent::Gossipsub(gossipsub::Event::Subscribed { peer_id, topic })) => {
                    app.add_message(format!("已订阅主题 {:?} 来自对等节点 {:?}", topic, peer_id));
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
                                app.add_message(format!("公共消息来自 {}: {}", chat_message.sender, chat_message.content));
                            }
                            MessageType::DirectMessage { recipient } => {
                                if recipient == app.app_state.nickname {
                                    app.add_message(format!("私信来自 {}: {}", chat_message.sender, chat_message.content));
                                }
                            }
                            MessageType::FileProposal => {
                                app.add_message(format!("文件提议来自 {}: {}", chat_message.sender, chat_message.content));
                            }
                            MessageType::CreateRoom { room } => {
                                app.app_state.rooms.insert(room.clone());
                                app.add_message(format!("房间 '{}' 由 {} 创建", room, chat_message.sender));
                            }
                            MessageType::JoinRoom { room } => {
                                app.add_message(format!("{} 加入了房间 '{}'", chat_message.sender, room));
                            }
                            MessageType::LeaveRoom { room } => {
                                app.add_message(format!("{} 离开了房间 '{}'", chat_message.sender, room));
                            }
                            MessageType::ListRooms => {
                                // 不做任何操作，因为我们不需要根据其他人的请求更新房间列表
                            }
                            MessageType::RoomMessage { room } => {
                                if app.app_state.joined_rooms.contains(&room) {
                                    app.add_message(format!("[{}] {} 说: {}", room, chat_message.sender, chat_message.content));
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
                SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                    app.add_message(format!("已建立与对等节点 {:?} 的连接", peer_id));
                }
                SwarmEvent::ConnectionClosed { peer_id, .. } => {
                    app.add_message(format!("与对等节点 {:?} 的连接已关闭", peer_id));
                }
                SwarmEvent::OutgoingConnectionError { peer_id, error, .. } => {
                    app.add_message(format!("连接到对等节点 {:?} 时出错: {:?}", peer_id, error));
                }
                SwarmEvent::IncomingConnectionError { send_back_addr, error, .. } => {
                    app.add_message(format!("来自 {:?} 的入站连接出错: {:?}", send_back_addr, error));
                }
                SwarmEvent::Dialing { peer_id, connection_id: _ } => {
                    app.add_message(format!("正在拨号对等节点 {:?}", peer_id));
                }
                _ => {}
            }
        }
        // 定期检查并显示连接状态
        if last_status_update.elapsed() > Duration::from_secs(30) {
            let connected_peers = swarm.connected_peers().count();
            app.add_message(format!("当前连接的对等节点数: {}", connected_peers));
            app.add_message(format!("已知的对等节点: {:?}", peer_nicknames.keys().collect::<Vec<_>>()));
            last_status_update = Instant::now();
        }
    }
     // 恢复终端
     disable_raw_mode()?;
     execute!(
         terminal.backend_mut(),
         LeaveAlternateScreen,
         DisableMouseCapture
     )?;
     terminal.show_cursor()?;
 
     Ok(())
}
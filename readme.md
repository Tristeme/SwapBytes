# Swapbytes
## Requirements Description
Implement a simple user interface (CLI) for users to send and receive messages and files. 
Peers have a user defined nickname and other users should be able to identify them by that in the UI (e.g., for direct messaging or file sharing). 
A pubsub chat room for users to propose the file they want, and what files they are willing to share. 
Peers that have connected for a trade in the chat room should be able to send direct messages to each other.  
Once peers have agreed to swap files they should be able to send the files to each other using a request/response pattern. 
You will need a mechanism to bootstrap your network, so peers can discover each other. You can use mDNS, but there should be some way to potentially connect to another peer not on the same local area network. You might use a rendezvous server for peer discovery for that. However, you do not need to implement NAT traversal (hole punching). 
Full documentation explaining how to run your program, including any command line parameters required and how to bootstrap the network. Use step-by-step examples and include them in your repo's README file. In addition, all commands defined for your program (e.g., posting to chat or sharing a file) must be clearly documented.
Add themed rooms that users can join to facilitate swaps. Users should be able to create a room.

## Getting Started

### Installation
```
# With Tensorflow CPU
cargo build --release
```

### Running the Application
```
# To run the application, use the following command:
cargo run --bin main -- --port <PORT> --nickname <NICKNAME> [--peer <PEER_ADDRESS>] [--bootstrap <BOOTSTRAP_NODE>]

```
### Command-line Options
- --nickname <NICKNAME>: Set your nickname (required)
- --port <PORT>: Specify the port to listen on (optional, default is random)
- --peer <PEER_ADDR>: Address of a peer to connect to (optional)
- --bootstrap <BOOTSTRAP_ADDR>: Address of a bootstrap node (optional)

Examples:
- Local Network (using mDNS):
```
cargo run -- --nickname Alice --port 50001
cargo run -- --nickname Bob --port 50002
cargo run -- --nickname Charlie --port 50003
```
- Wide Area Network (using bootstrap):
```
cargo run -- --nickname BootstrapNode --port 50001  # On server with public IP
cargo run -- --nickname Alice --bootstrap /ip4/<server_ip>/tcp/50001
cargo run -- --nickname Bob --bootstrap /ip4/<server_ip>/tcp/50001
```
- Hybrid Approach:
```
cargo run -- --nickname BootstrapNode --port 50001
cargo run -- --nickname Alice --port 50002 --bootstrap /ip4/127.0.0.1/tcp/50001
cargo run -- --nickname Bob --port 50003 --bootstrap /ip4/127.0.0.1/tcp/50001 --peer /ip4/127.0.0.1/tcp/50002
```


### Usages
- /msg <message>: Send a public message
- /dm <recipient_nickname> <message>: Send a direct message
- /propose <filename>: Propose a file to share
- /get <peer_nickname> <filename>: Request a file from a peer
- /create_room <room_name>: Create a new chat room
- /join_room <room_name>: Join an existing chat room
- /leave_room <room_name>: Leave a chat room
- /list_rooms: List all available rooms
- /room <room_name> <message>: Send a message in a specific room

examples:
Alice:
```
/msg hello, everyone! (every peer will receive it)
/dm Bob Hi, Bob (Bob peer will receive it)
/propose test.docx (every peer will receive it)
```

And then go to the Triste peer:
Bob:
```
/get test.docx (store a new copy file of test.docx)
/create_room room1 (every peer will receive it)
/join_room room1 (every peer will receive it)
```

And then go to the Charlie peer:
Charlie:
```
/list_rooms (will show: room1)
/join_room room1 (every peer will receive it)
/room room1 Hi, room1! (only the members in room1 will receive it)
```

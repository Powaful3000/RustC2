use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{atomic::{AtomicU64, Ordering}, Arc},
    io::Write,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{Mutex, mpsc::UnboundedSender},
    sync::broadcast,
};
use tokio::sync::mpsc;
use serde::{Serialize, Deserialize};
use bincode;

type SessionId = u64;
type Sessions = Arc<Mutex<HashMap<SessionId, mpsc::Sender<Vec<u8>>>>>;

#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    Command(String),
    Output(Vec<u8>),
    Error(String),
    Heartbeat,
}

#[derive(Clone)]
struct SessionManager {
    sessions: Sessions,
    next_id: Arc<AtomicU64>,
    output_tx: broadcast::Sender<String>,
}

impl SessionManager {
    fn new(output_tx: broadcast::Sender<String>) -> Self {
        SessionManager {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
            output_tx,
        }
    }

    async fn add_session(&self, writer: tokio::io::WriteHalf<TcpStream>) -> SessionId {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, mut rx) = mpsc::channel::<Vec<u8>>(100);
        
        tokio::spawn(async move {
            let mut writer = writer;
            while let Some(mut payload) = rx.recv().await {
                // Prepend length header (4 bytes little-endian)
                let len = payload.len() as u32;
                let mut len_bytes = len.to_le_bytes().to_vec();
                len_bytes.append(&mut payload);
                
                if let Err(e) = writer.write_all(&len_bytes).await {
                    eprintln!("[!] Error sending to session {}: {}", id, e);
                    break;
                }
            }
        });

        let mut sessions = self.sessions.lock().await;
        sessions.insert(id, tx);
        id
    }

    async fn remove_session(&self, id: SessionId) {
        let mut sessions = self.sessions.lock().await;
        sessions.remove(&id);
    }

    async fn list_sessions(&self) {
        let sessions = self.sessions.lock().await;
        println!("\nActive sessions ({}):\n", sessions.len());
        for id in sessions.keys() {
            println!("  - Session #{}", id);
        }
    }

    async fn get_session(&self, id: SessionId) -> Option<mpsc::Sender<Vec<u8>>> {
        let sessions = self.sessions.lock().await;
        sessions.get(&id).cloned()
    }
}

async fn handle_client(mgr: SessionManager, stream: TcpStream, addr: SocketAddr) {
    let (reader, writer) = tokio::io::split(stream);
    let id = mgr.add_session(writer).await;
    println!("[+] New connection from {} (Session #{})", addr, id);

    let mut reader = BufReader::new(reader);
    
    loop {
        // Read length prefix (4 bytes)
        let mut len_bytes = [0u8; 4];
        if let Err(e) = reader.read_exact(&mut len_bytes).await {
            eprintln!("[!] Session #{} read error: {}", id, e);
            break;
        }
        let len = u32::from_le_bytes(len_bytes) as usize;
        
        // Read exact message length
        let mut buf = vec![0u8; len];
        if let Err(e) = reader.read_exact(&mut buf).await {
            eprintln!("[!] Session #{} read error: {}", id, e);
            break;
        }

        // Handle the response
        match bincode::deserialize::<Message>(&buf) {
            Ok(msg) => match msg {
                Message::Output(output) => {
                    if let Ok(text) = String::from_utf8(output.clone()) {
                        println!("[Session #{}] {}", id, text);
                    } else {
                        println!("[Session #{}] Received binary output", id);
                    }
                }
                Message::Error(err) => {
                    println!("[Session #{}] Error: {}", id, err);
                }
                Message::Command(_) => {
                    println!("[Session #{}] Unexpected command message from client", id);
                }
                Message::Heartbeat => {
                    // Respond with heartbeat
                    if let Some(tx) = mgr.get_session(id).await {
                        let response = bincode::serialize(&Message::Heartbeat)
                            .expect("Failed to serialize heartbeat");
                        if tx.send(response).await.is_err() {
                            eprintln!("[!] Failed to send heartbeat response to session #{}", id);
                            break;
                        }
                    }
                }
            },
            Err(e) => {
                eprintln!("[!] Session #{} deserialization error: {}", id, e);
                break;
            }
        }
    }

    mgr.remove_session(id).await;
    println!("[-] Session #{} closed", id);
}

async fn start_server(mgr: SessionManager, addr: &str) -> tokio::io::Result<()> {
    let listener = TcpListener::bind(addr).await?;
    println!("[+] Server listening on {}", addr);

    loop {
        let (stream, addr) = listener.accept().await?;
        let mgr_clone = mgr.clone();
        
        tokio::spawn(async move {
            handle_client(mgr_clone, stream, addr).await
        });
    }
}

async fn interactive_shell(mgr: SessionManager) -> tokio::io::Result<()> {
    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut input = String::new();

    loop {
        // Print menu
        println!("\nMain Menu:");
        println!("1. List sessions");
        println!("2. Interact with session");
        println!("3. Exit");
        print!("> ");
        std::io::stdout().flush().unwrap();

        input.clear();
        stdin.read_line(&mut input).await?;
        let cmd = input.trim();

        match cmd {
            "1" => mgr.list_sessions().await,
            "2" => {
                print!("Enter session ID: ");
                std::io::stdout().flush().unwrap();
                
                input.clear();
                stdin.read_line(&mut input).await?;
                
                if let Ok(id) = input.trim().parse::<SessionId>() {
                    if let Some(tx) = mgr.get_session(id).await {
                        println!("[+] Interacting with session #{} (Type 'exit' to quit)", id);
                        
                        let mut output_rx = mgr.output_tx.subscribe();
                        let mut stdin = BufReader::new(tokio::io::stdin());
                        
                        loop {
                            print!("session-{}> ", id);
                            std::io::stdout().flush().unwrap();
                            
                            let mut input = String::new();
                            let read_line = async {
                                stdin.read_line(&mut input).await
                            };

                            tokio::select! {
                                result = read_line => {
                                    match result {
                                        Ok(_) => {
                                            let cmd = input.trim();
                                            if cmd == "exit" {
                                                break;
                                            }
                                            
                                            let msg = Message::Command(cmd.to_string());
                                            match bincode::serialize(&msg) {
                                                Ok(data) => {
                                                    if tx.send(data).await.is_err() {
                                                        println!("[!] Session #{} closed", id);
                                                        break;
                                                    }
                                                }
                                                Err(e) => println!("[!] Failed to serialize command: {}", e),
                                            }
                                        }
                                        Err(e) => {
                                            eprintln!("Error reading input: {}", e);
                                            break;
                                        }
                                    }
                                }
                                msg = output_rx.recv() => {
                                    if let Ok(msg) = msg {
                                        if msg.starts_with(&format!("[Session #{}]", id)) {
                                            print!("{}", msg);
                                            std::io::stdout().flush().unwrap();
                                        }
                                    }
                                }
                            }
                        }
                    } else {
                        println!("[!] Invalid session ID");
                    }
                } else {
                    println!("[!] Invalid input");
                }
            }
            "3" => break,
            _ => println!("[!] Invalid command"),
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> tokio::io::Result<()> {
    let (output_tx, _) = broadcast::channel::<String>(1024);
    
    let mgr = SessionManager::new(output_tx.clone());
    let server_addr = "0.0.0.0:8080";

    let print_task = tokio::spawn(async move {
        let mut output_rx = output_tx.subscribe();
        while let Ok(msg) = output_rx.recv().await {
            if msg.ends_with('\n') {
                print!("{}", msg);
            } else {
                println!("{}", msg);
            }
            std::io::stdout().flush().unwrap();
        }
    });

    let server_task = tokio::spawn({
        let mgr = mgr.clone();
        async move {
            if let Err(e) = start_server(mgr, server_addr).await {
                eprintln!("[!] Server error: {}", e);
            }
        }
    });

    if let Err(e) = interactive_shell(mgr).await {
        eprintln!("[!] Shell error: {}", e);
    }

    print_task.abort();
    server_task.abort();
    println!("[+] Server shutdown");
    Ok(())
}
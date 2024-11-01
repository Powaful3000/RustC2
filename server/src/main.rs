use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

fn handle_client(mut stream: TcpStream) {
    // Display server prompt initially
    display_server_prompt();

    // Initialize the working directory from the client's initial WORKDIR message
    let mut current_directory = read_initial_working_directory(&mut stream);

    loop {
        let mut command = String::new();

        // Display initial prompt without directory unless in shell mode
        print!("Server> ");
        io::stdout().flush().unwrap();
        io::stdin()
            .read_line(&mut command)
            .expect("Failed to read line");

        if command.trim() == "shell" {
            stream
                .write_all(b"shell")
                .expect("Failed to write to stream");
            println!("Entered shell mode. Type 'exit' to close the shell.");

            loop {
                let mut shell_command = String::new();
                print!("{}> ", current_directory); // Display updated prompt in shell mode
                io::stdout().flush().unwrap();
                io::stdin()
                    .read_line(&mut shell_command)
                    .expect("Failed to read line");

                if shell_command.trim() == "exit" {
                    stream
                        .write_all(b"exit")
                        .expect("Failed to write to stream");
                    println!("Exiting shell mode.");
                    break;
                }

                stream
                    .write_all(shell_command.as_bytes())
                    .expect("Failed to send shell command");

                // Process response and handle potential WORKDIR updates
                if !process_response(&mut stream, &mut current_directory) {
                    println!("Client disconnected.");
                    reset_to_default_prompt();
                    return;
                }
            }
        } else if command.trim() == "exit" {
            stream
                .write_all(b"exit")
                .expect("Failed to write to stream");
            println!("Closing connection.");
            reset_to_default_prompt();
            break;
        } else if command.trim().starts_with("exec") {
            stream
                .write_all(command.as_bytes())
                .expect("Failed to write to stream");

            // Process response and handle potential WORKDIR updates
            if !process_response(&mut stream, &mut current_directory) {
                println!("Client disconnected.");
                reset_to_default_prompt();
                return;
            }
        } else {
            println!("Unknown command. Please use 'shell', 'exec <command>', or 'exit'.");
        }
    }
}

// Function to read the initial working directory from the client
fn read_initial_working_directory(stream: &mut TcpStream) -> String {
    let mut initial_directory = String::from("Connected to client"); // Initial message instead of default directory
    let mut response = Vec::new();
    let mut buffer = vec![0; 4096];
    loop {
        let bytes_read = stream
            .read(&mut buffer)
            .expect("Failed to read initial WORKDIR");
        response.extend_from_slice(&buffer[..bytes_read]);

        if response.ends_with(b"<END>") {
            response.truncate(response.len() - 5); // Remove "<END>" delimiter
            break;
        }
    }

    if let Ok(response_text) = String::from_utf8(response) {
        if response_text.starts_with("WORKDIR:") {
            initial_directory = response_text[8..].to_string(); // Set initial directory
        }
    }

    initial_directory
}

// Function to process responses and check for WORKDIR updates or disconnections
fn process_response(stream: &mut TcpStream, current_directory: &mut String) -> bool {
    // Read response until "<END>" delimiter is found
    let mut response = Vec::new();
    let mut buffer = vec![0; 4096];
    loop {
        let bytes_read = match stream.read(&mut buffer) {
            Ok(0) => return false, // Client disconnected
            Ok(n) => n,
            Err(_) => return false, // Handle unexpected errors as disconnection
        };
        response.extend_from_slice(&buffer[..bytes_read]);

        if response.ends_with(b"<END>") {
            response.truncate(response.len() - 5); // Remove "<END>" delimiter
            break;
        }
    }

    // Check if the response is an updated working directory message
    if let Ok(response_text) = String::from_utf8(response.clone()) {
        if response_text.starts_with("WORKDIR:") {
            *current_directory = response_text[8..].to_string(); // Update current directory
        } else {
            println!("{}", response_text); // Print the command output
        }
    }

    true
}

// Function to reset the prompt to the default after disconnection
fn reset_to_default_prompt() {
    display_server_prompt(); // Display server commands and prompt
}

// Function to display server commands and initial prompt
fn display_server_prompt() {
    println!("Server> Available commands:");
    println!("  shell - Enter persistent shell mode with the client");
    println!("  exec <command> - Execute a single command on the client");
    println!("  exit  - Disconnect from the current client");
    // Removed redundant prompt print here
}

fn main() -> std::io::Result<()> {
    let listener = TcpListener::bind("127.0.0.1:7878")?;
    println!("Server running on port 7878");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                println!("Client connected");
                thread::spawn(|| handle_client(stream));
            }
            Err(e) => eprintln!("Connection failed: {}", e),
        }
    }
    Ok(())
}
